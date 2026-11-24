//! The transcripts of a workflow lead's shift, kept for its record: when a shift ends, each
//! computer copies the Claude Code transcripts of that shift's sessions it holds (its runners',
//! and on the coordinator the lead's own) into its private `implementation/archive/<shift>/`,
//! compressed, and says where they are, their size and SHA-256; nothing leaves the computer and
//! nothing is read or summarized. Archives older than `KEEP_DAYS` are removed when another is
//! written there. Claude Code removes its own transcripts after `cleanupPeriodDays`.
// shortcut: Claude Code's transcripts only; Codex and OpenCode sessions are recorded as not
// archived. Add their stores here when a workflow lead follows runs of theirs.
use super::*;
use sha2::Digest;
use std::io::{Read, Write};

/// Days an archive is kept.
pub(in crate::app) const KEEP_DAYS: u64 = 90;
/// The most sessions one request archives, and the largest transcript copied.
const SESSIONS: usize = 200;
const LARGEST: u64 = 512 * 1024 * 1024;

/// A shift's ID or a session's, as archives name them.
fn id(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|v| uuid::Uuid::parse_str(v).is_ok() && v.len() == 36)
        .map(str::to_owned)
}

/// `transcripts_archive {shift, sessions}`: what this computer archived of those sessions.
pub(in crate::app) async fn local(host: &Arc<Host>, args: &Value) -> Result<Value> {
    let shift = id(&args["shift"]).context("Give the shift's ID.")?;
    let sessions: Vec<String> = args["sessions"]
        .as_array()
        .context("Give the sessions.")?
        .iter()
        .take(SESSIONS)
        .filter_map(id)
        .collect();
    let root = host.state.implementation.root.join("archive");
    let projects = crate::sessions::Roots::from_env()
        .claude
        .map(|c| c.join("projects"));
    let archived = tokio::task::spawn_blocking(move || {
        prune(&root, std::time::SystemTime::now());
        let dir = root.join(&shift);
        security::private_dir(&dir)?;
        Ok::<_, anyhow::Error>(
            sessions
                .iter()
                .map(|session| copy(projects.as_deref(), &dir, session))
                .collect::<Vec<_>>(),
        )
    })
    .await??;
    Ok(json!({"transcripts": archived}))
}

/// Remove the archives of `root` older than `KEEP_DAYS`.
fn prune(root: &Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let keep = std::time::Duration::from_secs(KEEP_DAYS * 86_400);
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > keep));
        if old && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The transcript of `session` under Claude Code's `projects`, if this computer has it: a regular
/// file in a real directory, never through a link (a session could point its own transcript at
/// a secret).
fn locate(projects: Option<&Path>, session: &str) -> Option<PathBuf> {
    let regular = |path: &Path, dir: bool| {
        std::fs::symlink_metadata(path).is_ok_and(|m| {
            !m.file_type().is_symlink() && if dir { m.is_dir() } else { m.is_file() }
        })
    };
    std::fs::read_dir(projects?)
        .ok()?
        .flatten()
        .filter(|dir| regular(&dir.path(), true))
        .map(|dir| dir.path().join(format!("{session}.jsonl")))
        .find(|path| regular(path, false))
}

/// Copy one session's transcript, compressed, into `dir`: where it is, its size and SHA-256.
fn copy(projects: Option<&Path>, dir: &Path, session: &str) -> Value {
    let Some(source) = locate(projects, session) else {
        return json!({"session_id": session, "archived": false, "reason": "not on this computer"});
    };
    let target = dir.join(format!("{session}.jsonl.gz"));
    let copied = (|| -> Result<(u64, String)> {
        let file = std::fs::File::open(&source)?;
        ensure!(file.metadata()?.len() <= LARGEST, "larger than 512 MB");
        let out = std::fs::File::create(&target)?;
        security::protect_file(&target)?;
        let mut gz = flate2::write::GzEncoder::new(out, flate2::Compression::default());
        // Bounded while read too: a transcript still being written may grow meanwhile.
        let (mut reader, mut hash, mut size) = (
            std::io::BufReader::new(file).take(LARGEST + 1),
            sha2::Sha256::new(),
            0u64,
        );
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
            gz.write_all(&buffer[..read])?;
            size += read as u64;
        }
        ensure!(size <= LARGEST, "larger than 512 MB");
        gz.finish()?.sync_all()?;
        let digest = hash.finalize();
        Ok((size, digest.iter().map(|b| format!("{b:02x}")).collect()))
    })();
    match copied {
        Ok((size, sha256)) => json!({"session_id": session, "archived": true,
            "source": source.display().to_string(), "path": target.display().to_string(),
            "size": size, "sha256": sha256}),
        Err(error) => {
            let _ = std::fs::remove_file(&target);
            json!({"session_id": session, "archived": false, "reason": bounded(&error.to_string())})
        }
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(200).collect()
}

/// Archive the transcripts of an ended shift on every computer that holds them and record where
/// they are (`record.transcripts`): the lead's conversations on this coordinator, each runner's
/// session on its machine; a machine offline or too old is named with why.
pub(in crate::app) async fn shift(host: Arc<Host>, id: String) {
    if let Err(error) = record(&host, &id).await {
        tracing::warn!(event = "lead_archive_failed", shift = %id, error = %error);
    }
}

async fn record(host: &Arc<Host>, id: &str) -> Result<()> {
    let shifts = &host.state.workflows.shifts;
    let Some(shift) = shifts.get(id)? else {
        return Ok(());
    };
    // Each runner: the run and child its coding run was for.
    let mut coding: Vec<(String, Value)> = Vec::new();
    for run in &shift.followed {
        let Some(run) = host.state.workflows.db.get(run)? else {
            continue;
        };
        for child in &run.children {
            if let Some(started) = &child.coding {
                coding.push((
                    started.id.clone(),
                    json!({"who": "runner", "run": run.id, "child": child.id, "implementation_run": started.id}),
                ));
            }
        }
    }
    let ids: Vec<String> = coding.iter().map(|(id, _)| id.clone()).collect();
    let (own, sessions) = implementation::sessions_of(host, &ids).await;
    // Who held each session: (machine, session, what it was).
    let mut wanted: Vec<(String, String, Value)> = Vec::new();
    for session in shift.conversations.iter().chain(shift.session_id.iter()) {
        if !wanted.iter().any(|(_, s, _)| s == session) {
            wanted.push((own.clone(), session.clone(), json!({"who": "lead"})));
        }
    }
    for (machine, session, run) in sessions {
        if let Some((_, about)) = coding.iter().find(|(id, _)| *id == run) {
            wanted.push((machine, session, about.clone()));
        }
    }
    let mut machines: Vec<String> = wanted.iter().map(|(m, _, _)| m.clone()).collect();
    machines.sort();
    machines.dedup();
    let mut transcripts = Vec::new();
    for machine in machines {
        let sessions: Vec<&String> = wanted
            .iter()
            .filter(|(m, _, _)| *m == machine)
            .map(|(_, s, _)| s)
            .collect();
        let args = json!({"shift": shift.id, "sessions": sessions});
        let (name, reachable) = implementation::archive_reach(host, &machine).await;
        let answer = if machine == own {
            local(host, &args).await.map_err(|e| e.to_string())
        } else if reachable {
            implementation::call_machine(
                host,
                &machine,
                "transcripts_archive",
                args,
                std::time::Duration::from_secs(120),
            )
            .await
            .map_err(|e| e.to_string())
        } else {
            Err("offline, or too old to archive transcripts".into())
        };
        for (_, session, about) in wanted.iter().filter(|(m, _, _)| *m == machine) {
            let mut entry = about.clone();
            entry["machine"] = json!(name);
            entry["session_id"] = json!(session);
            match &answer {
                Ok(reply) => {
                    if let Some(found) = reply["transcripts"]
                        .as_array()
                        .and_then(|t| t.iter().find(|t| t["session_id"] == session.as_str()))
                    {
                        for key in ["archived", "reason", "source", "path", "size", "sha256"] {
                            if !found[key].is_null() {
                                entry[key] = found[key].clone();
                            }
                        }
                    } else {
                        entry["archived"] = json!(false);
                        entry["reason"] = json!("not on that computer");
                    }
                }
                Err(reason) => {
                    entry["archived"] = json!(false);
                    entry["reason"] = json!(bounded(reason));
                }
            }
            transcripts.push(entry);
        }
    }
    // Read again: only the record's transcripts change, whatever else was written meanwhile.
    // shortcut: a shift whose archive was cut by a restart keeps no transcripts list; archive
    // ended shifts without one at a later tick if that matters.
    let Some(mut shift) = shifts.get(id)? else {
        return Ok(());
    };
    if let Some(record) = shift.record.as_mut() {
        record["transcripts"] = json!(transcripts);
        record["archive_days"] = json!(KEEP_DAYS);
    }
    shifts.save(&shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transcript_is_copied_compressed_with_its_size_and_hash_and_old_archives_go() {
        let home = tempfile::tempdir().unwrap();
        let projects = home.path().join("projects");
        let session = "0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f";
        std::fs::create_dir_all(projects.join("-work-payments")).unwrap();
        let transcript = b"{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n";
        std::fs::write(
            projects
                .join("-work-payments")
                .join(format!("{session}.jsonl")),
            transcript,
        )
        .unwrap();
        let root = home.path().join("archive");
        let dir = root.join("shift");
        security::private_dir(&dir).unwrap();
        let copied = copy(Some(&projects), &dir, session);
        assert_eq!(copied["archived"], true, "{copied}");
        assert_eq!(copied["size"], transcript.len());
        assert_eq!(copied["sha256"], crate::hash::sha256_hex(transcript));
        let mut unzipped = String::new();
        flate2::read::GzDecoder::new(
            std::fs::File::open(copied["path"].as_str().unwrap()).unwrap(),
        )
        .read_to_string(&mut unzipped)
        .unwrap();
        assert_eq!(unzipped.as_bytes(), transcript);
        let missing = copy(
            Some(&projects),
            &dir,
            "1f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f",
        );
        assert_eq!(missing["archived"], false);
        // Never through a link.
        #[cfg(unix)]
        {
            let linked = "2f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f";
            std::fs::write(home.path().join("secret"), b"token").unwrap();
            std::os::unix::fs::symlink(
                home.path().join("secret"),
                projects
                    .join("-work-payments")
                    .join(format!("{linked}.jsonl")),
            )
            .unwrap();
            assert_eq!(copy(Some(&projects), &dir, linked)["archived"], false);
        }
        // Kept 90 days: a later archive removes older ones.
        prune(
            &root,
            std::time::SystemTime::now() + std::time::Duration::from_secs(91 * 86_400),
        );
        assert!(!dir.exists());
    }
}
