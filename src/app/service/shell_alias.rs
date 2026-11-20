//! The `kk` shell alias for `kuroko`: one marked block in the user's bash, zsh and PowerShell
//! profiles, written the first time the host of a version starts (a first installation or a new
//! version), so `kk status` runs `kuroko status`. It is an alias the shell expands, not another
//! executable. A profile that already defines `kk` its own way is left alone; the block is
//! replaced in place, never duplicated, and `install.sh --uninstall` / `install.ps1 -Uninstall`
//! remove it.

use std::path::{Path, PathBuf};

pub(crate) const NAME: &str = "kk";
pub(crate) const BEGIN: &str = "# >>> kuroko alias >>>";
pub(crate) const END: &str = "# <<< kuroko alias <<<";
/// The version whose start last wrote the alias, in the profile.
const MARKER: &str = "shell-alias.json";
/// Set to any value, the host writes no alias (development and test hosts).
const OPT_OUT: &str = "KUROKO_NO_SHELL_ALIAS";
/// The files bash and zsh read besides the profiles written, where a user may define `kk`.
const ALIAS_FILES: [&str; 8] = [
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".bash_aliases",
    ".aliases",
    ".zsh_aliases",
    ".profile",
    ".zshenv",
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Syntax {
    /// bash and zsh.
    Posix,
    PowerShell,
}
impl Syntax {
    fn line(self) -> String {
        match self {
            Self::Posix => format!("alias {NAME}='kuroko'"),
            Self::PowerShell => format!("Set-Alias -Name {NAME} -Value kuroko"),
        }
    }
    /// Whether `line`, outside this block, already defines `kk` its own way.
    fn defines(self, line: &str) -> bool {
        let line = line.trim();
        match self {
            Self::Posix => {
                line.starts_with(&format!("alias {NAME}="))
                    || line.starts_with(&format!("{NAME}()"))
                    || line.starts_with(&format!("{NAME} ()"))
                    || line.starts_with(&format!("function {NAME} "))
                    || line.starts_with(&format!("function {NAME}("))
            }
            Self::PowerShell => {
                let lower = line.to_lowercase();
                (lower.starts_with("set-alias") || lower.starts_with("new-alias"))
                    && lower.split_whitespace().any(|word| word == NAME)
                    || lower.starts_with(&format!("function {NAME} "))
                    || lower.starts_with(&format!("function {NAME}{{"))
            }
        }
    }
}

/// Where the profiles are, for this user on this system.
struct Home {
    home: PathBuf,
    windows: bool,
    /// The login shell's name (`zsh`, `bash`), from `SHELL`.
    shell: Option<String>,
    /// The Documents folder PowerShell reads its profiles from (Windows).
    documents: Option<PathBuf>,
    /// Whether Windows PowerShell runs a profile script (its execution policy allows local
    /// scripts); without it a new profile would print an error in every window.
    scripts: bool,
    /// Whether PowerShell 7 is installed (macOS and Linux keep its profile under `~/.config`).
    pwsh: bool,
}
impl Home {
    fn current() -> Option<Self> {
        let windows = cfg!(windows);
        let home = std::env::var_os(if windows { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())?;
        let shell = std::env::var("SHELL").ok().and_then(|s| {
            Path::new(&s)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        });
        let documents = windows.then(|| documents(&home));
        let scripts = windows && scripts_allowed();
        let pwsh = on_path(if windows { "pwsh.exe" } else { "pwsh" });
        Some(Self {
            home,
            windows,
            shell,
            documents,
            scripts,
            pwsh,
        })
    }
    /// The profiles to write: each bash or zsh startup file that exists (the login shell's own
    /// is created on macOS and Linux), and PowerShell's current-user profiles: one that exists,
    /// or a new one where that PowerShell runs it (PowerShell 7 installed; Windows PowerShell
    /// with an execution policy that allows local scripts). Nothing for bash and zsh when a file
    /// they read defines `kk` already (`~/.bash_aliases`, for example).
    fn profiles(&self) -> Vec<(PathBuf, Syntax)> {
        let mut profiles = Vec::new();
        for file in [".bashrc", ".bash_profile", ".zshrc"] {
            let path = self.home.join(file);
            let own = !self.windows
                && match self.shell.as_deref() {
                    Some("zsh") => file == ".zshrc",
                    Some("bash") => file == ".bashrc",
                    _ => false,
                };
            if own || path.is_file() {
                profiles.push((path, Syntax::Posix));
            }
        }
        if ALIAS_FILES.iter().any(|file| {
            let (outside, _) =
                split(&std::fs::read_to_string(self.home.join(file)).unwrap_or_default());
            outside.lines().any(|line| Syntax::Posix.defines(line))
        }) {
            profiles.clear();
        }
        if let Some(documents) = &self.documents {
            for (edition, runs) in [
                ("PowerShell", self.pwsh),
                ("WindowsPowerShell", self.scripts),
            ] {
                let path = documents.join(edition).join("profile.ps1");
                if runs || path.is_file() {
                    profiles.push((path, Syntax::PowerShell));
                }
            }
        } else if self.pwsh || self.home.join(".config/powershell").is_dir() {
            profiles.push((
                self.home.join(".config/powershell/profile.ps1"),
                Syntax::PowerShell,
            ));
        }
        profiles
    }
}

/// The Documents folder as PowerShell resolves `$PROFILE` (it may be redirected, to OneDrive
/// for example), or `%USERPROFILE%\Documents` when PowerShell does not say.
fn documents(home: &Path) -> PathBuf {
    let asked = crate::security::program("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::GetFolderPath('MyDocuments')",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .filter(|p| p.is_absolute());
    asked.unwrap_or_else(|| home.join("Documents"))
}

/// Whether Windows PowerShell's effective execution policy runs a local, unsigned profile.
fn scripts_allowed() -> bool {
    crate::security::program("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-ExecutionPolicy",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            ["RemoteSigned", "Unrestricted", "Bypass"]
                .contains(&String::from_utf8_lossy(&o.stdout).trim())
        })
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// What happened to one profile.
#[derive(Debug, PartialEq)]
enum Outcome {
    Written,
    Unchanged,
    /// The profile defines `kk` its own way, or holds a block without its end.
    Taken,
}

/// Write the block into `path`, replacing one already there; leave a profile that defines `kk`
/// outside the block untouched. A linked profile is written through its link.
fn write(path: &Path, syntax: Syntax) -> std::io::Result<Outcome> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let (outside, inside) = split(&text);
    // A block whose end was removed or edited leaves the profile as the person left it.
    let broken = inside
        .as_deref()
        .is_some_and(|b| !b.lines().any(|l| l.trim_end() == END));
    if broken || outside.lines().any(|line| syntax.defines(line)) {
        return Ok(Outcome::Taken);
    }
    let block = format!("{BEGIN}\n{}\n{END}\n", syntax.line());
    if inside.as_deref() == Some(block.as_str()) {
        return Ok(Outcome::Unchanged);
    }
    let mut new = outside;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    if !new.is_empty() && !new.ends_with("\n\n") {
        new.push('\n');
    }
    new.push_str(&block);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, new)?;
    Ok(Outcome::Written)
}

/// The text without the block (and the blank line before it), and the block itself.
fn split(text: &str) -> (String, Option<String>) {
    let mut outside = String::new();
    let mut inside: Option<String> = None;
    let mut within = false;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\r', '\n']);
        if !within && bare == BEGIN {
            within = true;
            inside = Some(String::new());
            // The blank line written before the block goes with it.
            if outside.ends_with("\n\n") {
                outside.pop();
            }
        }
        if within {
            inside.get_or_insert_with(String::new).push_str(line);
            if bare == END {
                within = false;
            }
        } else {
            outside.push_str(line);
        }
    }
    (outside, inside)
}

/// Write the alias once per version: on the first start of a newly installed or updated host.
/// Development builds and a host started with `KUROKO_NO_SHELL_ALIAS` write nothing.
pub(crate) fn ensure(profile: &Path) {
    if cfg!(debug_assertions) || std::env::var_os(OPT_OUT).is_some() {
        return;
    }
    let marker = profile.join(MARKER);
    let version = env!("CARGO_PKG_VERSION");
    let written = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v["version"] == version);
    if written {
        return;
    }
    let Some(home) = Home::current() else {
        return;
    };
    for (path, syntax) in home.profiles() {
        match write(&path, syntax) {
            Ok(outcome) => {
                tracing::info!(event = "shell_alias", path = %path.display(), outcome = ?outcome)
            }
            Err(error) => {
                tracing::warn!(event = "shell_alias_failed", path = %path.display(), %error)
            }
        }
    }
    let _ = std::fs::write(&marker, serde_json::json!({"version": version}).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(dir: &Path, windows: bool, shell: Option<&str>) -> Home {
        Home {
            home: dir.to_path_buf(),
            windows,
            shell: shell.map(str::to_owned),
            documents: windows.then(|| dir.join("Documents")),
            scripts: windows,
            pwsh: windows,
        }
    }

    #[test]
    fn the_login_shell_gets_its_file_and_existing_ones_are_written() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".bashrc"), "export A=1\n").unwrap();
        let profiles = home(dir.path(), false, Some("zsh")).profiles();
        let names: Vec<_> = profiles
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [".bashrc", ".zshrc"]);
        for (path, syntax) in profiles {
            assert_eq!(write(&path, syntax).unwrap(), Outcome::Written);
            assert_eq!(write(&path, syntax).unwrap(), Outcome::Unchanged);
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".bashrc")).unwrap(),
            format!("export A=1\n\n{BEGIN}\nalias kk='kuroko'\n{END}\n")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".zshrc")).unwrap(),
            format!("{BEGIN}\nalias kk='kuroko'\n{END}\n")
        );
    }

    #[test]
    fn windows_writes_both_powershell_profiles_and_only_existing_bash_files() {
        let dir = tempfile::tempdir().unwrap();
        let profiles = home(dir.path(), true, None).profiles();
        assert_eq!(
            profiles,
            [
                (
                    dir.path().join("Documents/PowerShell/profile.ps1"),
                    Syntax::PowerShell
                ),
                (
                    dir.path().join("Documents/WindowsPowerShell/profile.ps1"),
                    Syntax::PowerShell
                ),
            ]
        );
        for (path, syntax) in profiles {
            assert_eq!(write(&path, syntax).unwrap(), Outcome::Written);
            assert!(
                std::fs::read_to_string(&path)
                    .unwrap()
                    .contains("Set-Alias -Name kk -Value kuroko")
            );
        }
    }

    #[test]
    fn an_old_block_is_replaced_in_place_and_an_own_kk_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".zshrc");
        std::fs::write(&path, format!("a\n\n{BEGIN}\nalias kk='old'\n{END}\nb\r\n")).unwrap();
        assert_eq!(write(&path, Syntax::Posix).unwrap(), Outcome::Written);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("a\nb\r\n\n{BEGIN}\nalias kk='kuroko'\n{END}\n")
        );
        let own = dir.path().join(".bashrc");
        std::fs::write(&own, "alias kk='kubectl'\n").unwrap();
        assert_eq!(write(&own, Syntax::Posix).unwrap(), Outcome::Taken);
        assert_eq!(
            std::fs::read_to_string(&own).unwrap(),
            "alias kk='kubectl'\n"
        );
        let ps = dir.path().join("profile.ps1");
        std::fs::write(&ps, "Set-Alias kk Get-ChildItem\n").unwrap();
        assert_eq!(write(&ps, Syntax::PowerShell).unwrap(), Outcome::Taken);
        // A block whose end is gone: nothing after it is lost.
        let cut = dir.path().join(".cut");
        let text = format!("a\n{BEGIN}\nalias kk='kuroko'\nexport B=2\n");
        std::fs::write(&cut, &text).unwrap();
        assert_eq!(write(&cut, Syntax::Posix).unwrap(), Outcome::Taken);
        assert_eq!(std::fs::read_to_string(&cut).unwrap(), text);
    }

    #[test]
    fn a_kk_in_an_aliases_file_and_a_restricted_powershell_get_no_new_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".bashrc"), "export A=1\n").unwrap();
        std::fs::write(dir.path().join(".bash_aliases"), "alias kk='kubectl'\n").unwrap();
        let mut restricted = home(dir.path(), true, None);
        restricted.scripts = false;
        restricted.pwsh = false;
        assert!(restricted.profiles().is_empty());
        // A profile that exists is still written: it already runs, or already fails.
        let existing = dir.path().join("Documents/WindowsPowerShell/profile.ps1");
        std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
        std::fs::write(&existing, "").unwrap();
        assert_eq!(restricted.profiles(), [(existing, Syntax::PowerShell)]);
    }
}
