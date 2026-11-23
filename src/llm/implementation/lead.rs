//! One turn of a workflow lead's conversation: Claude Code on the coordinator with no tool of its
//! own (`--tools ""`, `--safe-mode`, `--strict-mcp-config`), whose only tools are the lead's,
//! served in this process as the SDK MCP server `kuroko` over the stream-JSON control channel.
//! A first turn starts the conversation; a later one resumes it by its session ID. The turn ends
//! at its `result`, or at `limit`, which stops it.
use super::*;
use anyhow::Context;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// What one turn gave.
#[derive(Debug, Default)]
pub(crate) struct LeadTurn {
    /// The conversation it continued or started.
    pub(crate) session_id: Option<String>,
    /// Its final text, bounded and redacted.
    pub(crate) text: String,
    /// The input tokens its last request carried (the conversation's size), when reported.
    pub(crate) context_tokens: Option<u64>,
}

/// The CLI's arguments for a lead's conversation: no tool of its own, its tools served in this
/// process, `resume`d when it has one.
pub(crate) fn arguments((model, effort): (&str, &str), resume: Option<&str>) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--safe-mode",
        "--strict-mcp-config",
        "--tools",
        "",
        "--permission-mode",
        "bypassPermissions",
        "--model",
        model,
        "--effort",
        effort,
    ]
    .map(OsString::from)
    .into();
    if let Some(session) = resume {
        args.extend(["--resume".into(), session.into()]);
    }
    args
}

/// Run one turn of a lead's conversation in `directory` with `prompt`.
pub(crate) async fn lead_turn(
    (model, effort): (&str, &str),
    directory: &Path,
    resume: Option<&str>,
    prompt: &str,
    server: Arc<Reporter>,
    limit: Duration,
) -> Result<LeadTurn> {
    let binary = locate("claude").ok_or_else(unavailable)?;
    let args = arguments((model, effort), resume);
    let mut command = crate::security::program_async(&binary);
    command
        .args(&args)
        .current_dir(directory)
        .env_clear()
        .envs(agent::child_env(&binary))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| Failed::with(Cause::CliMissing, "The lead's CLI did not start."))?;
    let _tree = ProcessTree(
        child
            .id()
            .ok_or_else(|| anyhow::anyhow!("The lead's CLI did not start."))?,
    );
    let mut input = child.stdin.take().context("The lead's CLI has no input.")?;
    let mut output = BufReader::new(
        child
            .stdout
            .take()
            .context("The lead's CLI has no output.")?,
    );
    let mut lines = lines::Lines::new();
    let send = |value: Value| format!("{value}\n");
    let initialize = json!({"type":"control_request","request_id":"lead-initialize","request":{"subtype":"initialize","sdkMcpServers":["kuroko"]}});
    let user = json!({"type":"user","message":{"role":"user","content":prompt},"parent_tool_use_id":null,"session_id":resume.unwrap_or("")});
    let mut turn = LeadTurn {
        session_id: resume.map(str::to_owned),
        ..Default::default()
    };
    // The limit covers the whole turn, writing its message included; the message goes once the
    // CLI has its tools.
    let followed = tokio::time::timeout(limit, async {
        input.write_all(send(initialize).as_bytes()).await?;
        let mut user = Some(user);
        loop {
            let bytes = output.fill_buf().await?;
            let (used, line) = if bytes.is_empty() {
                (0, lines.finish())
            } else {
                lines.feed(bytes)
            };
            let ended = bytes.is_empty();
            output.consume(used);
            if let Some(lines::Line::Event(event)) = line {
                match event["type"].as_str() {
                    // A CLI that refuses the lead's tools would answer without them.
                    Some("control_response")
                        if event["response"]["request_id"] == "lead-initialize" =>
                    {
                        ensure!(
                            event["response"]["subtype"] == "success",
                            Failed::with(Cause::AgentError, "The lead's CLI refused its tools.")
                        );
                        if let Some(user) = user.take() {
                            input.write_all(send(user).as_bytes()).await?;
                        }
                    }
                    // The conversation's size: what its latest request carried.
                    Some("assistant") => {
                        let usage = &event["message"]["usage"];
                        let counts: Vec<u64> = [
                            "input_tokens",
                            "cache_read_input_tokens",
                            "cache_creation_input_tokens",
                        ]
                        .iter()
                        .filter_map(|k| usage[k].as_u64())
                        .collect();
                        if !counts.is_empty() {
                            turn.context_tokens = Some(counts.iter().sum());
                        }
                    }
                    Some("control_request") => {
                        let request = &event["request"];
                        let reply = if request["subtype"] == "mcp_message"
                            && request["server_name"] == "kuroko"
                        {
                            let response = mobile::mcp_reply(&server, &request["message"]).await;
                            json!({"type":"control_response","response":{"subtype":"success","request_id":event["request_id"],
                                "response":{"mcp_response": response}}})
                        } else {
                            // Nobody answers a lead's prompt: anything else is refused.
                            json!({"type":"control_response","response":{"subtype":"error","request_id":event["request_id"],
                                "error":"This host does not grant tool approvals."}})
                        };
                        input.write_all(send(reply).as_bytes()).await?;
                    }
                    Some("system") if event["subtype"] == "init" => {
                        if let Some(id) = event["session_id"].as_str().filter(|id| !id.is_empty()) {
                            turn.session_id = Some(id.to_owned());
                        }
                    }
                    Some("result") => {
                        if let Some(id) = event["session_id"].as_str().filter(|id| !id.is_empty()) {
                            turn.session_id = Some(id.to_owned());
                        }

                        ensure!(
                            event["is_error"] != true,
                            Failed::with(Cause::AgentError, "The lead's turn ended in an error.")
                        );
                        turn.text = report_text(event["result"].as_str().unwrap_or_default())?;
                        return Ok::<_, anyhow::Error>(());
                    }
                    _ => {}
                }
            }
            ensure!(
                !ended,
                Failed::with(Cause::StreamEnded, "The lead's CLI ended before its turn did.")
            );
        }
    })
    .await;
    // Its transcript is written before the process ends: it is waited for briefly.
    drop(input);
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    let _ = child.start_kill();
    match followed {
        Ok(result) => result.map(|()| turn),
        Err(_) => Err(Failed::with(
            Cause::Timeout,
            "The lead's turn reached its time limit and was stopped.",
        )),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A fake Claude CLI: answers the initialize, calls the lead's tool once, then ends its turn
    /// with a result in session `ses-lead`; it records its arguments and what it was sent.
    const CLI: &str = r#"#!/bin/sh
printf '%s\n' "$@" > '{log}/arguments'
read -r initialize; printf '%s\n' "$initialize" > '{log}/initialize'
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"lead-initialize","response":{}}}'
read -r user; printf '%s\n' "$user" > '{log}/user'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"ses-lead"}'
printf '%s\n' '{"type":"control_request","request_id":"m1","request":{"subtype":"mcp_message","server_name":"kuroko","message":{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"lead_note","arguments":{"text":"Looked."}}}}}'
read -r reply; printf '%s\n' "$reply" > '{log}/reply'
printf '%s\n' '{"type":"assistant","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":90}}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"Nothing to decide yet.","session_id":"ses-lead","usage":{"input_tokens":30,"cache_read_input_tokens":270}}'
"#;

    #[tokio::test]
    async fn a_lead_turn_serves_its_tools_in_process_and_ends_at_its_result() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path();
        let cli = log.join("claude");
        agent::install_fake_cli(&cli, &CLI.replace("{log}", &log.display().to_string()));
        use_test_cli(Some(cli));
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = calls.clone();
        let server = Arc::new(Reporter::sync(
            json!([{"name": "lead_note"}]),
            move |tool: &str, args: &Value| {
                seen.lock().unwrap().push((tool.to_owned(), args.clone()));
                Ok(json!({"noted": true}))
            },
        ));
        let turn = lead_turn(
            ("model", "high"),
            log,
            Some("ses-lead"),
            "Events: none.",
            server,
            Duration::from_secs(20),
        )
        .await
        .unwrap();
        use_test_cli(None);
        assert_eq!(turn.session_id.as_deref(), Some("ses-lead"));
        assert_eq!(turn.text, "Nothing to decide yet.");
        assert_eq!(turn.context_tokens, Some(100));
        assert_eq!(calls.lock().unwrap()[0].0, "lead_note");
        let arguments = std::fs::read_to_string(log.join("arguments")).unwrap();
        for expected in [
            "--safe-mode",
            "--strict-mcp-config",
            "--tools",
            "--resume\nses-lead",
        ] {
            assert!(arguments.contains(expected), "{arguments}");
        }
        let initialize: Value =
            serde_json::from_str(&std::fs::read_to_string(log.join("initialize")).unwrap())
                .unwrap();
        assert_eq!(initialize["request"]["sdkMcpServers"], json!(["kuroko"]));
        let reply: Value =
            serde_json::from_str(&std::fs::read_to_string(log.join("reply")).unwrap()).unwrap();
        assert_eq!(reply["response"]["response"]["mcp_response"]["id"], 7);
        assert_eq!(
            reply["response"]["response"]["mcp_response"]["result"]["isError"],
            false
        );
    }
}
