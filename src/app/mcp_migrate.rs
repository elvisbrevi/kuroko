//! `kuroko mcp migrate`: moves this computer's coding-agent configurations from the MCP server name
//! `personal-teams-assistant` to `kuroko` (docs/rename-kuroko.md, phase 3). A client names tools
//! after the key its configuration registers the server under (`mcp__<key>__<tool>`), so the new
//! `serverInfo.name` alone renames nothing.
//!
//! It reads Claude Code's `~/.claude.json` (user and project scopes) and permission lists
//! (`~/.claude/settings.json`) and Codex's `~/.codex/config.toml`. It changes a registration only
//! through that client's own CLI (`claude mcp`, `codex mcp`), never by editing a file the client
//! rewrites while it runs; the permission list is rewritten atomically, keeping every other entry.
//! Without `--confirmed` it only prints the plan.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(super) const FORMER: &str = "personal-teams-assistant";
pub(super) const CURRENT: &str = "kuroko";

/// One registration of the former name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Found {
    pub client: &'static str,
    /// `user`, or the project directory of a project-scoped Claude Code entry.
    pub scope: String,
    /// `stdio` (a command) or `http` (a URL).
    pub transport: &'static str,
    /// The command or the URL.
    pub target: String,
}

/// What `migrate` would do: the commands, in order, and the permission entries to rename.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Plan {
    pub found: Vec<Found>,
    pub commands: Vec<Vec<String>>,
    pub permissions: Option<(PathBuf, usize)>,
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// The registrations of the former name in Claude Code's configuration document.
pub(super) fn claude_entries(document: &Value) -> Vec<Found> {
    let entry = |scope: String, server: &Value| {
        let (transport, target) = match server["url"].as_str() {
            Some(url) => ("http", url.to_owned()),
            None => (
                "stdio",
                server["command"].as_str().unwrap_or_default().to_owned(),
            ),
        };
        Found {
            client: "claude",
            scope,
            transport,
            target,
        }
    };
    let mut found = Vec::new();
    if let Some(server) = document["mcpServers"].get(FORMER) {
        found.push(entry("user".into(), server));
    }
    if let Some(projects) = document["projects"].as_object() {
        for (dir, project) in projects {
            if let Some(server) = project["mcpServers"].get(FORMER) {
                found.push(entry(dir.clone(), server));
            }
        }
    }
    found
}

/// The registration of the former name in Codex's configuration, if any.
pub(super) fn codex_entry(document: &toml::Table) -> Option<Found> {
    let server = document
        .get("mcp_servers")?
        .as_table()?
        .get(FORMER)?
        .as_table()?;
    let (transport, target) = match server.get("url").and_then(toml::Value::as_str) {
        Some(url) => ("http", url.to_owned()),
        None => (
            "stdio",
            server
                .get("command")
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
    };
    Some(Found {
        client: "codex",
        scope: "user".into(),
        transport,
        target,
    })
}

/// The command a migrated stdio registration starts: the `kuroko` beside the former `pta`, when
/// the registration named `pta` and that file exists; otherwise the same command.
pub(super) fn current_command(command: &str) -> String {
    let path = Path::new(command);
    let renamed = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| *stem == "pta")
        .map(|_| path.with_file_name(crate::maintenance::exe_name(CURRENT)))
        .filter(|candidate| candidate.is_file());
    renamed.map_or_else(|| command.to_owned(), |p| p.to_string_lossy().into_owned())
}

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// The CLI commands that replace one registration with the same one under the current name.
pub(super) fn commands(found: &Found) -> Vec<Vec<String>> {
    let added = |client: &str, rest: Vec<String>| -> Vec<String> {
        let mut command = words(&[client, "mcp", "add"]);
        command.extend(rest);
        command
    };
    match found.client {
        "claude" => {
            let scope = if found.scope == "user" {
                "user"
            } else {
                "project"
            };
            let mut remove = words(&["claude", "mcp", "remove", FORMER, "--scope", scope]);
            let mut add = if found.transport == "http" {
                added(
                    "claude",
                    words(&[
                        "--transport",
                        "http",
                        "--scope",
                        scope,
                        CURRENT,
                        &found.target,
                    ]),
                )
            } else {
                added(
                    "claude",
                    words(&[
                        "--scope",
                        scope,
                        CURRENT,
                        "--",
                        &current_command(&found.target),
                        "mcp",
                    ]),
                )
            };
            if scope == "project" {
                // Project-scoped entries are changed from inside that project.
                remove.insert(0, format!("cd:{}", found.scope));
                add.insert(0, format!("cd:{}", found.scope));
            }
            vec![remove, add]
        }
        _ => {
            let remove = words(&["codex", "mcp", "remove", FORMER]);
            let add = if found.transport == "http" {
                added("codex", words(&[CURRENT, "--url", &found.target]))
            } else {
                added(
                    "codex",
                    words(&[CURRENT, "--", &current_command(&found.target), "mcp"]),
                )
            };
            vec![remove, add]
        }
    }
}

/// A permission list with every `mcp__personal-teams-assistant__…` entry renamed (and nothing
/// duplicated), and how many entries were renamed.
pub(super) fn renamed_permissions(settings: &Value) -> (Value, usize) {
    let mut settings = settings.clone();
    let mut count = 0;
    let former = format!("mcp__{FORMER}__");
    let former_bare = format!("mcp__{FORMER}");
    for list in ["allow", "deny", "ask"] {
        let Some(entries) = settings["permissions"][list].as_array_mut() else {
            continue;
        };
        let mut next: Vec<Value> = Vec::new();
        for entry in entries.iter() {
            let renamed = entry.as_str().and_then(|text| {
                text.strip_prefix(&former)
                    .map(|rest| format!("mcp__{CURRENT}__{rest}"))
                    .or_else(|| (text == former_bare).then(|| format!("mcp__{CURRENT}")))
            });
            let value = match renamed {
                Some(text) => {
                    count += 1;
                    Value::String(text)
                }
                None => entry.clone(),
            };
            if !next.contains(&value) {
                next.push(value);
            }
        }
        *entries = next;
    }
    (settings, count)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// What this computer has under the former name, and how to move it.
pub(super) fn plan() -> Plan {
    let Some(home) = home() else {
        return Plan::default();
    };
    let mut found = read_json(&home.join(".claude.json"))
        .map(|document| claude_entries(&document))
        .unwrap_or_default();
    let codex = std::fs::read_to_string(home.join(".codex/config.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|document| codex_entry(&document));
    found.extend(codex);
    let commands = found.iter().flat_map(commands).collect();
    let settings = home.join(".claude/settings.json");
    let permissions = read_json(&settings)
        .map(|document| renamed_permissions(&document).1)
        .filter(|count| *count > 0)
        .map(|count| (settings, count));
    Plan {
        found,
        commands,
        permissions,
    }
}

fn shown(command: &[String]) -> String {
    match command.first().and_then(|first| first.strip_prefix("cd:")) {
        Some(dir) => format!("(in {dir}) {}", command[1..].join(" ")),
        None => command.join(" "),
    }
}

fn run(command: &[String]) -> Result<()> {
    let (dir, command) = match command.first().and_then(|first| first.strip_prefix("cd:")) {
        Some(dir) => (Some(dir.to_owned()), &command[1..]),
        None => (None, command),
    };
    let program =
        crate::maintenance::find_program(&command[0], std::env::var_os("PATH").as_deref())
            .with_context(|| format!("{} was not found on PATH", command[0]))?;
    let mut process = std::process::Command::new(program);
    process
        .args(&command[1..])
        .stdin(std::process::Stdio::null());
    if let Some(dir) = dir {
        process.current_dir(dir);
    }
    let output = process.output()?;
    anyhow::ensure!(
        output.status.success(),
        "`{}` failed: {}",
        command.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

/// The reply of `kuroko mcp migrate`: the plan, and with `confirmed` its result.
pub(super) fn migrate(confirmed: bool) -> Value {
    let plan = plan();
    let mut done: Vec<String> = Vec::new();
    let mut error = None;
    if confirmed {
        for command in &plan.commands {
            match run(command) {
                Ok(()) => done.push(shown(command)),
                Err(e) => {
                    error = Some(e.to_string());
                    break;
                }
            }
        }
        if error.is_none()
            && let Some((path, _)) = &plan.permissions
            && let Some(document) = read_json(path)
        {
            let (renamed, _) = renamed_permissions(&document);
            let written = serde_json::to_string_pretty(&renamed)
                .map_err(anyhow::Error::from)
                .and_then(|text| {
                    let temp =
                        path.with_extension(format!("json.{}.new", uuid::Uuid::new_v4().simple()));
                    std::fs::write(&temp, format!("{text}\n"))?;
                    std::fs::rename(&temp, path)?;
                    Ok(())
                });
            match written {
                Ok(()) => done.push(format!("renamed the permissions in {}", path.display())),
                Err(e) => error = Some(format!("The permission list could not be rewritten: {e}")),
            }
        }
    }
    json!({
        "former": FORMER,
        "current": CURRENT,
        "found": plan.found.iter().map(|f| json!({"client": f.client, "scope": f.scope, "transport": f.transport, "target": f.target})).collect::<Vec<_>>(),
        "commands": plan.commands.iter().map(|c| shown(c)).collect::<Vec<_>>(),
        "permissions": plan.permissions.as_ref().map(|(path, count)| json!({"file": path, "entries": count})),
        "applied": confirmed,
        "done": done,
        "error": error,
        "next": if plan.found.is_empty() && plan.permissions.is_none() {
            "Nothing on this computer uses the former name.".to_owned()
        } else if confirmed && error.is_none() {
            "Restart the coding agents (or reconnect their MCP servers) so their tools appear as mcp__kuroko__*.".to_owned()
        } else if confirmed {
            "Fix the cause and run kuroko mcp migrate --confirmed again; finished steps are not repeated.".to_owned()
        } else {
            "Run kuroko mcp migrate --confirmed to apply these commands, or run them yourself.".to_owned()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_registrations_of_the_former_name_are_found_in_every_scope() {
        let document = json!({
            "mcpServers": {FORMER: {"type": "stdio", "command": "/u/.cargo/bin/pta", "args": ["mcp"]}, "other": {}},
            "projects": {
                "/work/a": {"mcpServers": {FORMER: {"type": "http", "url": "https://portal.example/mcp"}}},
                "/work/b": {"mcpServers": {"other": {}}},
            }
        });
        let found = claude_entries(&document);
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].scope.as_str(), found[0].transport),
            ("user", "stdio")
        );
        assert_eq!(
            (found[1].scope.as_str(), found[1].transport),
            ("/work/a", "http")
        );
        assert!(claude_entries(&json!({"mcpServers": {"kuroko": {}}})).is_empty());
    }

    #[test]
    fn each_registration_is_replaced_through_its_clients_own_cli() {
        let stdio = Found {
            client: "claude",
            scope: "user".into(),
            transport: "stdio",
            target: "/nowhere/bin/pta".into(),
        };
        assert_eq!(
            commands(&stdio),
            [
                words(&["claude", "mcp", "remove", FORMER, "--scope", "user"]),
                words(&[
                    "claude",
                    "mcp",
                    "add",
                    "--scope",
                    "user",
                    CURRENT,
                    "--",
                    "/nowhere/bin/pta",
                    "mcp"
                ]),
            ]
        );
        let http = Found {
            client: "claude",
            scope: "/work/a".into(),
            transport: "http",
            target: "https://portal.example/mcp".into(),
        };
        let plan = commands(&http);
        assert_eq!(plan[0][0], "cd:/work/a");
        assert_eq!(
            &plan[1][1..],
            words(&[
                "claude",
                "mcp",
                "add",
                "--transport",
                "http",
                "--scope",
                "project",
                CURRENT,
                "https://portal.example/mcp"
            ])
        );
        let codex = codex_entry(
            &"[mcp_servers.personal-teams-assistant]\ncommand = \"/x/pta\"\nargs = [\"mcp\"]\n"
                .parse()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            commands(&codex),
            [
                words(&["codex", "mcp", "remove", FORMER]),
                words(&["codex", "mcp", "add", CURRENT, "--", "/x/pta", "mcp"]),
            ]
        );
        assert!(codex_entry(&"[mcp_servers.kuroko]\ncommand = \"k\"\n".parse().unwrap()).is_none());
    }

    #[test]
    fn a_registered_pta_becomes_the_kuroko_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let pta = dir.path().join(crate::maintenance::exe_name("pta"));
        std::fs::write(&pta, b"x").unwrap();
        // Without a kuroko beside it, the command is kept.
        assert_eq!(
            current_command(&pta.to_string_lossy()),
            pta.to_string_lossy()
        );
        let kuroko = dir.path().join(crate::maintenance::exe_name("kuroko"));
        std::fs::write(&kuroko, b"x").unwrap();
        assert_eq!(
            current_command(&pta.to_string_lossy()),
            kuroko.to_string_lossy()
        );
        assert_eq!(current_command("/opt/other"), "/opt/other");
    }

    #[test]
    fn permissions_are_renamed_without_touching_other_entries() {
        let settings = json!({
            "model": "x",
            "permissions": {
                "allow": [format!("mcp__{FORMER}__status"), "Bash(ls)", format!("mcp__{CURRENT}__status"), format!("mcp__{FORMER}")],
                "deny": [format!("mcp__{FORMER}__teams_post")],
            }
        });
        let (renamed, count) = renamed_permissions(&settings);
        assert_eq!(count, 3);
        assert_eq!(renamed["model"], "x");
        assert_eq!(
            renamed["permissions"]["allow"],
            json!([
                format!("mcp__{CURRENT}__status"),
                "Bash(ls)",
                format!("mcp__{CURRENT}")
            ])
        );
        assert_eq!(
            renamed["permissions"]["deny"],
            json!([format!("mcp__{CURRENT}__teams_post")])
        );
        assert_eq!(
            renamed_permissions(&json!({"permissions": {"allow": ["Bash(ls)"]}})).1,
            0
        );
    }
}
