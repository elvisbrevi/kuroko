//! Helpers shared by unit tests.

/// Writes an executable shell script at `path` through a child process, so this process never
/// holds it open for writing. Otherwise another test's fork in that moment inherits the
/// descriptor until its exec, and running the script fails with ETXTBSY ("text file busy"),
/// which made tests fail now and then on Linux.
#[cfg(unix)]
pub(crate) fn executable(path: &std::path::Path, script: &str) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut writer = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("cat > \"$0\" && chmod 755 \"$0\"")
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(writer.wait().unwrap().success(), "{}", path.display());
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn a_written_script_runs_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/run.sh");
        super::executable(&path, "#!/bin/sh\necho ran\n");
        let output = std::process::Command::new(&path).output().unwrap();
        assert_eq!(output.stdout, b"ran\n");
    }
}
