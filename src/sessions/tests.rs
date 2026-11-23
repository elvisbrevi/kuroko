use super::*;
use serde_json::json;
use std::io::Write as _;

fn write_lines(path: &Path, lines: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::File::create(path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}
/// Set a file's modification time `minutes` after a fixed instant, so order is known.
fn touch(path: &Path, minutes: u64) {
    let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_793_000_000 + minutes * 60);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}
fn at(minutes: u64) -> String {
    from_millis(((1_793_000_000 + minutes * 60) * 1000) as i64).unwrap()
}

fn claude_session(root: &Path, id: &str, minutes: u64, asked: &str, answer: &str) {
    let path = root.join(format!("projects/-home-user-src-payments/{id}.jsonl"));
    write_lines(
        &path,
        &[
            json!({"type":"summary","summary":"Payments","leafUuid":"x"}),
            json!({"type":"user","isMeta":true,"sessionId":id,"cwd":"/home/user/src/payments","message":{"role":"user","content":"<command-name>/clear</command-name>"},"timestamp":at(minutes)}),
            json!({"type":"user","sessionId":id,"cwd":"/home/user/src/payments","message":{"role":"user","content":asked},"timestamp":at(minutes)}),
            json!({"type":"assistant","sessionId":id,"message":{"role":"assistant","content":[{"type":"text","text":"Reading the code."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/home/user/src/payments/.env"}}]},"timestamp":at(minutes)}),
            json!({"type":"user","sessionId":id,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"FILE CONTENT SHOULD NEVER APPEAR"}]},"timestamp":at(minutes)}),
            json!({"type":"assistant","isSidechain":true,"sessionId":id,"message":{"role":"assistant","content":[{"type":"text","text":"SIDECHAIN TEXT"}]},"timestamp":at(minutes)}),
            json!({"type":"assistant","sessionId":id,"message":{"role":"assistant","content":[{"type":"text","text":answer}]},"timestamp":at(minutes)}),
        ],
    );
    touch(&path, minutes);
}
fn codex_session(root: &Path, id: &str, minutes: u64, asked: &str, answer: &str) {
    let path = root.join(format!(
        "sessions/2026/11/12/rollout-2026-11-12T10-00-00-{id}.jsonl"
    ));
    write_lines(
        &path,
        &[
            json!({"timestamp":at(minutes),"type":"session_meta","payload":{"id":id,"timestamp":at(minutes),"cwd":"/srv/work/billing","originator":"codex_cli_rs"}}),
            json!({"timestamp":at(minutes),"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd</environment_context>"}]}}),
            json!({"timestamp":at(minutes),"type":"event_msg","payload":{"type":"user_message","message":asked}}),
            json!({"timestamp":at(minutes),"type":"response_item","payload":{"type":"function_call_output","output":"COMMAND OUTPUT SHOULD NEVER APPEAR"}}),
            json!({"timestamp":at(minutes),"type":"event_msg","payload":{"type":"agent_message","message":answer}}),
        ],
    );
    touch(&path, minutes);
}
fn opencode_session(root: &Path, id: &str, minutes: u64, asked: &str, answer: &str) {
    let storage = root.join("storage");
    let ms = ((1_793_000_000 + minutes * 60) * 1000) as i64;
    let info = storage.join(format!("session/project1/{id}.json"));
    std::fs::create_dir_all(info.parent().unwrap()).unwrap();
    std::fs::write(
        &info,
        json!({"id":id,"title":"Queue","directory":"/home/user/queue","time":{"created":ms,"updated":ms}}).to_string(),
    )
    .unwrap();
    touch(&info, minutes);
    for (n, (role, text)) in [("user", asked), ("assistant", answer)].iter().enumerate() {
        let message = format!("msg_{id}_{n}");
        let dir = storage.join(format!("message/{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{message}.json")),
            json!({"id":message,"role":role,"time":{"created":ms + n as i64}}).to_string(),
        )
        .unwrap();
        let parts = storage.join(format!("part/{message}"));
        std::fs::create_dir_all(&parts).unwrap();
        std::fs::write(
            parts.join("prt_1.json"),
            json!({"id":"prt_1","type":"text","text":text}).to_string(),
        )
        .unwrap();
        std::fs::write(
            parts.join("prt_2.json"),
            json!({"id":"prt_2","type":"tool","state":{"output":"TOOL OUTPUT SHOULD NEVER APPEAR"}}).to_string(),
        )
        .unwrap();
    }
}
fn roots(dir: &Path) -> Roots {
    Roots {
        home: None,
        claude: Some(dir.join("claude")),
        codex: Some(dir.join("codex")),
        opencode: Some(dir.join("opencode")),
    }
}

#[test]
fn the_most_recent_sessions_across_the_three_clis_come_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    // Twelve sessions spread over the three CLIs, at known times.
    for n in 0..4u64 {
        claude_session(
            roots.claude.as_ref().unwrap(),
            &format!("claude-{n}"),
            n * 3,
            &format!("Fix the payments retry {n}"),
            "The retry now waits; the deploy is still pending.",
        );
        codex_session(
            roots.codex.as_ref().unwrap(),
            &format!("codex-{n}"),
            n * 3 + 1,
            &format!("Check the billing export {n}"),
            "The export works.",
        );
        opencode_session(
            roots.opencode.as_ref().unwrap(),
            &format!("ses_{n}"),
            n * 3 + 2,
            &format!("Drain the queue {n}"),
            "Drained.",
        );
    }
    let listing = local(&roots, 10, "laptop", &Filter::default());
    assert!(listing.unavailable.is_empty(), "{:?}", listing.unavailable);
    let order: Vec<_> = listing
        .sessions
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert_eq!(
        order,
        [
            "ses_3", "codex-3", "claude-3", "ses_2", "codex-2", "claude-2", "ses_1", "codex-1",
            "claude-1", "ses_0"
        ]
    );
    let claude = listing
        .sessions
        .iter()
        .find(|s| s.session_id == "claude-3")
        .unwrap();
    assert_eq!(claude.provider, "claude");
    assert_eq!(claude.machine, "laptop");
    assert_eq!(claude.project, "payments");
    assert_eq!(claude.asked, "Fix the payments retry 3");
    assert_eq!(
        claude.concluded,
        "The retry now waits; the deploy is still pending."
    );
    assert_eq!(
        claude.open,
        "The retry now waits; the deploy is still pending"
    );
    assert_eq!(claude.started_at, at(9));
    let codex = listing
        .sessions
        .iter()
        .find(|s| s.session_id == "codex-3")
        .unwrap();
    assert_eq!(
        (codex.project.as_str(), codex.asked.as_str()),
        ("billing", "Check the billing export 3")
    );
    let opencode = listing
        .sessions
        .iter()
        .find(|s| s.session_id == "ses_3")
        .unwrap();
    assert_eq!(
        (opencode.project.as_str(), opencode.concluded.as_str()),
        ("queue", "Drained.")
    );
    // Tool calls, their results and side conversations are never read.
    let text = serde_json::to_string(&listing).unwrap();
    for never in [
        "SHOULD NEVER APPEAR",
        "SIDECHAIN",
        "/home/user",
        ".env",
        "environment_context",
    ] {
        assert!(!text.contains(never), "{never}");
    }
    // The number asked for is respected.
    assert_eq!(
        local(&roots, 3, "laptop", &Filter::default())
            .sessions
            .len(),
        3
    );
    assert_eq!(
        local(&roots, 50, "laptop", &Filter::default())
            .sessions
            .len(),
        12
    );
}

#[test]
fn secrets_never_reach_a_summary_or_its_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let token = "tok_9fK2pQ7xL4mN8vR3sT6wY1zA5bC0dE2gH7jK"; // gitleaks:allow
    let connection =
        "Server=tcp:db.example.test,1433;Database=pay;User Id=app;Password=Synthetic-Pass-123;"; // gitleaks:allow
    let key = format!(
        "-----BEGIN {} KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7\nsyntheticline\n-----END {} KEY-----",
        "PRIVATE", "PRIVATE"
    );
    let asked = format!(
        "Try the connection to the payments database from QA.\nIts string is {connection}\nUse the token {token} for the API.\n{key}\nexport DB_PASSWORD=hunter-two"
    );
    let answer = format!(
        "The QA connection times out at the firewall; I asked for the port to be opened and am waiting for it.\nI used {token}.\n```\nPassword=in-a-code-block\n```\npostgres://admin:Synthetic-Pass@db.example.test/pay"
    );
    claude_session(roots.claude.as_ref().unwrap(), "c1", 1, &asked, &answer);
    codex_session(roots.codex.as_ref().unwrap(), "x1", 2, &asked, &answer);
    opencode_session(
        roots.opencode.as_ref().unwrap(),
        "ses_1",
        3,
        &asked,
        &answer,
    );
    let listing = local(&roots, 10, "laptop", &Filter::default());
    assert_eq!(listing.sessions.len(), 3);
    let keys: Vec<(String, &Summary)> = listing
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| (format!("s{}", i + 1), s))
        .collect();
    let stored = serde_json::to_string(&listing).unwrap();
    let sent = evidence(&keys);
    for text in [&stored, &sent] {
        for secret in [
            token,
            "Synthetic-Pass",
            "Password=",
            "MIIEvQ",
            "syntheticline",
            "PRIVATE KEY",
            "hunter-two",
            "in-a-code-block",
            "db.example.test",
        ] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
    for session in &listing.sessions {
        assert_eq!(
            session.asked,
            "Try the connection to the payments database from QA."
        );
        assert!(
            session
                .concluded
                .starts_with("The QA connection times out at the firewall"),
            "{}",
            session.concluded
        );
        assert!(session.open.contains("waiting for it"), "{}", session.open);
    }
    assert!(sent.contains("Working session s1"));
    assert!(sent.contains("machine laptop"));
}

#[test]
fn private_and_empty_sessions_are_left_out_and_an_unreadable_store_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    claude_session(
        roots.claude.as_ref().unwrap(),
        "kept",
        1,
        "Review the queue",
        "Done.",
    );
    claude_session(
        roots.claude.as_ref().unwrap(),
        "hidden",
        2,
        "#private look at my notes",
        "Done.",
    );
    // An empty Codex store adds nothing.
    let codex = roots.codex.as_ref().unwrap();
    std::fs::create_dir_all(codex.join("sessions")).unwrap();
    // An OpenCode database of another layout is unreadable, never empty.
    let opencode = roots.opencode.as_ref().unwrap();
    std::fs::create_dir_all(opencode).unwrap();
    rusqlite::Connection::open(opencode.join("opencode.db"))
        .unwrap()
        .execute_batch("CREATE TABLE other(id TEXT);")
        .unwrap();
    let listing = local(&roots, 10, "laptop", &Filter::default());
    let ids: Vec<_> = listing
        .sessions
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert_eq!(ids, ["kept"]);
    assert_eq!(
        listing.unavailable,
        [Unavailable {
            machine: "laptop".into(),
            provider: "opencode".into(),
            reason: "unreadable".into()
        }]
    );
}

#[test]
fn an_opencode_database_is_read_without_its_tool_parts_or_sub_agents() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("opencode");
    std::fs::create_dir_all(&root).unwrap();
    let db = rusqlite::Connection::open(root.join("opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE session(id TEXT, parent_id TEXT, directory TEXT, title TEXT, time_created INTEGER, time_updated INTEGER);
         CREATE TABLE message(id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
         CREATE TABLE part(id TEXT, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
         INSERT INTO session VALUES('ses_a', NULL, '/home/user/gateway', 'Gateway', 1793000000000, 1793000600000);
         INSERT INTO session VALUES('ses_child', 'ses_a', '/home/user/gateway', 'Sub', 1793000000000, 1793000900000);
         INSERT INTO message VALUES('m1', 'ses_a', 1793000000000, '{\"role\":\"user\"}');
         INSERT INTO message VALUES('m2', 'ses_a', 1793000500000, '{\"role\":\"assistant\"}');
         INSERT INTO part VALUES('p1', 'm1', 'ses_a', 1, '{\"type\":\"text\",\"text\":\"Why does the gateway return 502?\"}');
         INSERT INTO part VALUES('p2', 'm2', 'ses_a', 2, '{\"type\":\"tool\",\"state\":{\"output\":\"TOOL OUTPUT\"}}');
         INSERT INTO part VALUES('p3', 'm2', 'ses_a', 3, '{\"type\":\"text\",\"text\":\"The upstream pool is exhausted.\"}');",
    )
    .unwrap();
    let listing = local(
        &Roots {
            opencode: Some(root),
            ..Default::default()
        },
        10,
        "server",
        &Filter::default(),
    );
    assert_eq!(listing.sessions.len(), 1);
    let session = &listing.sessions[0];
    assert_eq!(session.session_id, "ses_a");
    assert_eq!(session.project, "gateway");
    assert_eq!(session.asked, "Why does the gateway return 502?");
    assert_eq!(session.concluded, "The upstream pool is exhausted.");
    assert_eq!(session.updated_at, from_millis(1_793_000_600_000).unwrap());
}

#[test]
fn listings_merge_newest_first_and_name_the_runs_pta_started() {
    let summary = |id: &str, at: &str| Summary {
        provider: "claude".into(),
        session_id: id.into(),
        updated_at: at.into(),
        ..Default::default()
    };
    let mut merged = Listing::merge(
        [
            Listing {
                sessions: vec![summary("a", "2026-11-12T10:00:00Z")],
                ..Default::default()
            },
            Listing {
                sessions: vec![
                    summary("b", "2026-11-12T11:00:00Z"),
                    summary("a", "2026-11-12T10:00:00Z"),
                ],
                unavailable: vec![Unavailable {
                    machine: "desk".into(),
                    provider: String::new(),
                    reason: "offline".into(),
                }],
                ..Default::default()
            },
        ],
        10,
    );
    assert_eq!(
        merged
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
    assert_eq!(merged.unavailable[0].reason, "offline");
    merged.attribute(&[(
        "a".into(),
        Started {
            run: "run-1".into(),
            work_item_url: "https://dev.azure.com/org/proj/_workitems/edit/7".into(),
            work_item_title: "Retry payments".into(),
            state: "running".into(),
        },
    )]);
    assert_eq!(merged.sessions[1].started_by.as_ref().unwrap().run, "run-1");
    // The evidence says a run started it, but names no work item: an answer may name one only
    // with its verified link, and the prompt forbids naming them.
    let text = evidence(&[("s1".into(), &merged.sessions[1])]);
    assert!(text.contains("Started by the assistant") && text.contains("run state running"));
    assert!(!text.contains("Retry payments") && !text.contains("dev.azure.com"));
    assert!(crate::llm::prompts::IN_PROGRESS.contains("never name or number a work item"));
}

#[test]
fn a_long_session_is_read_from_its_start_and_its_end_and_long_lines_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude/projects/p/long.jsonl");
    let mut lines = vec![
        json!({"type":"user","sessionId":"long","cwd":"/w/p","message":{"role":"user","content":"First request"},"timestamp":at(0)}),
    ];
    let image = "A".repeat(400_000);
    for _ in 0..12 {
        lines.push(json!({"type":"user","sessionId":"long","message":{"role":"user","content":[{"type":"tool_result","content":[{"type":"image","source":{"data":image}}]}]},"timestamp":at(1)}));
    }
    lines.push(json!({"type":"assistant","sessionId":"long","message":{"role":"assistant","content":[{"type":"text","text":"Final answer"}]},"timestamp":at(5)}));
    write_lines(&path, &lines);
    assert!(std::fs::metadata(&path).unwrap().len() > HEAD_BYTES + TAIL_BYTES);
    let listing = local(
        &Roots {
            claude: Some(dir.path().join("claude")),
            ..Default::default()
        },
        10,
        "",
        &Filter::default(),
    );
    let session = &listing.sessions[0];
    assert_eq!(
        (session.asked.as_str(), session.concluded.as_str()),
        ("First request", "Final answer")
    );
    assert_eq!(session.updated_at, at(5));
}

fn claude_lines(root: &Path, id: &str, lines: &[Value]) -> PathBuf {
    let path = root.join(format!("claude/projects/p/{id}.jsonl"));
    write_lines(&path, lines);
    path
}
fn user(id: &str, text: &str) -> Value {
    json!({"type":"user","sessionId":id,"cwd":"/w/p","message":{"role":"user","content":text},"timestamp":at(1)})
}
fn agent(id: &str, text: &str) -> Value {
    json!({"type":"assistant","sessionId":id,"message":{"role":"assistant","content":[{"type":"text","text":text}]},"timestamp":at(2)})
}
fn claude_only(dir: &Path) -> Roots {
    Roots {
        claude: Some(dir.join("claude")),
        ..Default::default()
    }
}

#[test]
fn the_private_mark_is_seen_in_any_form_and_anywhere_in_the_session() {
    for form in [
        "#private!",
        "#private?",
        "\"#private\"",
        "**#private**",
        "`#private`",
        "(#PRIVATE)",
    ] {
        assert!(
            marked_private(&format!("look at this {form} please")),
            "{form}"
        );
    }
    assert!(!marked_private("#privateness"));
    assert!(!marked_private("see #private-notes"));
    let dir = tempfile::tempdir().unwrap();
    // In the unread middle of a long file.
    let mut lines = vec![user("middle", "First request")];
    let filler = "x".repeat(200_000);
    for _ in 0..8 {
        lines.push(agent("middle", &filler));
    }
    lines.push(user("middle", "keep this #private!"));
    for _ in 0..8 {
        lines.push(agent("middle", &filler));
    }
    lines.push(agent("middle", "Final answer"));
    let path = claude_lines(dir.path(), "middle", &lines);
    assert!(std::fs::metadata(&path).unwrap().len() > HEAD_BYTES + TAIL_BYTES);
    // In a user line too long to parse.
    claude_lines(
        dir.path(),
        "long",
        &[
            user("long", "First request"),
            user("long", &format!("{} #private", "y".repeat(300_000))),
            agent("long", "Done"),
        ],
    );
    // Said by the agent, not the user: not private.
    claude_lines(
        dir.path(),
        "said",
        &[
            user("said", "How do I hide a session?"),
            agent("said", "Write #private in a request."),
        ],
    );
    let listing = local(&claude_only(dir.path()), 10, "m", &Filter::default());
    let ids: Vec<_> = listing
        .sessions
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert_eq!(ids, ["said"]);
    assert!(listing.unavailable.is_empty());
}

#[test]
fn secrets_in_every_shape_never_reach_a_summary() {
    let scrubber = Scrubber::new();
    let base64 =
        "q9Zk3+Lm8/Rt5Wx2Yb7/Nc4Vd6Fh1Jg0/Kp2Ls9Mn3Bv5Cx7Zq1We4Rt6Yu8Io0Pa2Sd4Fg6Hj8Kl0Zx2Cv4Bn6==";
    let text = format!(
        "Before.\n\"DB_PASSWORD\": \"hunter2\"\n{{\"secret\": \"hunter3\", \"auth\": \"dXNlcjpwYXNz\"}}\nPassword:\nHunter4!\nrun it with --password Hunter5\nthe password is Hunter6!\nla contraseña es Hunter7\nla clave: Hunter8\nAccountKey={base64}\nstorage key {base64}\nAfter."
    );
    let kept = scrubber.scrub(&text);
    assert_eq!(kept, "Before. After.");
    let open = scrubber.pending(
        "Rotated most of them.\nexport API_KEY=sk-live-abc.def123 is still not yet rotated\nThe deploy is still pending.", // gitleaks:allow
    );
    assert_eq!(open, "The deploy is still pending");
    // Ordinary words are kept (who did something is not: see the work item test).
    assert_eq!(
        scrubber.scrub("Reviewer notes. Tests passed: 12. Design: done."),
        "Reviewer notes. Tests passed: 12. Design: done."
    );
}

#[test]
fn code_blocks_and_injected_text_are_dropped_by_their_own_markers() {
    let scrubber = Scrubber::new();
    assert_eq!(
        scrubber.scrub("Wrap it in ``` fences.\n```\nlet key = 1;\n```\nThen deploy."),
        "Wrap it in ``` fences. Then deploy."
    );
    assert_eq!(
        scrubber.scrub("Outer:\n````\n```\ninner code\n```\nstill code\n````\nDone."),
        "Outer: Done."
    );
    assert_eq!(
        scrubber.scrub(
            "<system-reminder>Note <example>x</example> INJECTED TAIL</system-reminder>Real request <div>kept</div>"
        ),
        "Real request <div>kept</div>"
    );
    // Shell output, task notifications and compaction summaries are not the user's requests.
    let dir = tempfile::tempdir().unwrap();
    let mut compact = user("c", "Summary of the earlier conversation");
    compact["isCompactSummary"] = json!(true);
    claude_lines(
        dir.path(),
        "c",
        &[
            user("c", "<bash-input>cat notes.txt</bash-input>"),
            user(
                "c",
                "<bash-stdout>SHELL OUTPUT</bash-stdout><bash-stderr></bash-stderr>",
            ),
            user("c", "<task-notification>TASK DONE</task-notification>"),
            compact,
            user("c", "Fix the retry"),
            agent("c", "Fixed."),
        ],
    );
    let listing = local(&claude_only(dir.path()), 10, "m", &Filter::default());
    let session = &listing.sessions[0];
    assert_eq!(
        (session.asked.as_str(), session.latest.as_str()),
        ("Fix the retry", "")
    );
    let text = serde_json::to_string(&listing).unwrap();
    for never in [
        "SHELL OUTPUT",
        "TASK DONE",
        "earlier conversation",
        "notes.txt",
    ] {
        assert!(!text.contains(never), "{never}");
    }
}

#[test]
fn absolute_paths_keep_only_their_last_component() {
    let scrubber = Scrubber::new();
    for (text, kept) in [
        (
            r"Saved C:\Users\First Last\Clients\Acme Corp\plan.docx today",
            "Saved plan.docx today",
        ),
        (
            "Open /Users/name/Library/Application Support/Acme/x now",
            "Open x now",
        ),
        ("cwd:/home/name/clients/acme", "cwd:acme"),
        (
            "see file:///home/name/clients/notes.txt",
            "see file://notes.txt",
        ),
        ("<p>/home/name/clients/y</p>", "<p>y</p>"),
        ("@/home/name/clients/z", "@z"),
        (
            "Docs at https://dev.azure.com/org/proj/_git/repo?path=/home/x",
            "Docs at https://dev.azure.com/org/proj/_git/repo?path=/home/x",
        ),
    ] {
        assert_eq!(scrubber.scrub(text), kept, "{text}");
    }
}

#[test]
fn a_store_or_session_that_cannot_be_read_is_unavailable_not_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("opencode");
    std::fs::create_dir_all(&root).unwrap();
    let db = rusqlite::Connection::open(root.join("opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE session(id TEXT, parent_id TEXT, directory TEXT, time_created INTEGER, time_updated INTEGER);
         CREATE TABLE message(id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
         CREATE TABLE part(id TEXT, message_id TEXT, session_id TEXT, data TEXT);
         INSERT INTO session VALUES('ses_bad', NULL, '/w/a', 1793000000000, 1793000600000);
         INSERT INTO message VALUES('m1', 'ses_bad', 1793000000000, 'not json');
         INSERT INTO session VALUES('ses_ns', NULL, '/w/b', 1793000000000000000, 1793000600000000000);
         INSERT INTO message VALUES('m2', 'ses_ns', 1793000000000000000, '{\"role\":\"user\"}');
         INSERT INTO part VALUES('p2', 'm2', 'ses_ns', '{\"type\":\"text\",\"text\":\"Hello\"}');",
    )
    .unwrap();
    let listing = local(
        &Roots {
            opencode: Some(root),
            ..Default::default()
        },
        10,
        "m",
        &Filter::default(),
    );
    assert!(listing.sessions.is_empty());
    assert_eq!(
        listing.unavailable,
        [Unavailable {
            machine: "m".into(),
            provider: "opencode".into(),
            reason: "unreadable".into()
        }]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = claude_lines(
            dir.path(),
            "locked",
            &[user("locked", "Hi"), agent("locked", "Hello")],
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Only meaningful when permissions bind (not as root).
        if std::fs::File::open(&path).is_err() {
            let listing = local(&claude_only(dir.path()), 10, "m", &Filter::default());
            assert!(listing.sessions.is_empty());
            assert_eq!(listing.unavailable[0].reason, "unreadable");
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn opencode_reads_its_latest_messages_and_marks_beyond_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("opencode");
    std::fs::create_dir_all(&root).unwrap();
    let mut db = rusqlite::Connection::open(root.join("opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE session(id TEXT, parent_id TEXT, directory TEXT, time_created INTEGER, time_updated INTEGER);
         CREATE TABLE message(id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
         CREATE TABLE part(id TEXT, message_id TEXT, session_id TEXT, data TEXT);
         INSERT INTO session VALUES('ses_long', NULL, '/w/long', 1793000000000, 1793009000000);
         INSERT INTO session VALUES('ses_marked', NULL, '/w/marked', 1793000000000, 1793008000000);",
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    for (session, count) in [("ses_long", 2100), ("ses_marked", 200)] {
        for n in 0..count {
            let role = if n % 2 == 0 { "user" } else { "assistant" };
            let message = format!("{session}-m{n:05}");
            let text = match (session, n) {
                (_, 0) => "First request".to_owned(),
                ("ses_marked", 100) => "this one is #private!".to_owned(),
                (_, n) if n == count - 1 => "Latest answer".to_owned(),
                (_, n) => format!("message {n}"),
            };
            tx.execute(
                "INSERT INTO message VALUES(?1, ?2, ?3, ?4)",
                rusqlite::params![
                    message,
                    session,
                    1_793_000_000_000i64 + n as i64,
                    json!({"role":role}).to_string()
                ],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO part VALUES(?1, ?2, ?3, ?4)",
                rusqlite::params![
                    format!("{message}-p1"),
                    message,
                    session,
                    json!({"type":"text","text":text}).to_string()
                ],
            )
            .unwrap();
            if n == count - 1 {
                // A large tool part is never loaded.
                tx.execute(
                    "INSERT INTO part VALUES(?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        format!("{message}-p0"),
                        message,
                        session,
                        json!({"type":"tool","state":{"output":"T".repeat(300_000)}}).to_string()
                    ],
                )
                .unwrap();
            }
        }
    }
    tx.commit().unwrap();
    let listing = local(
        &Roots {
            opencode: Some(root),
            ..Default::default()
        },
        10,
        "m",
        &Filter::default(),
    );
    assert!(listing.unavailable.is_empty(), "{:?}", listing.unavailable);
    let ids: Vec<_> = listing
        .sessions
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert_eq!(ids, ["ses_long"]);
    let session = &listing.sessions[0];
    assert_eq!(
        (session.asked.as_str(), session.concluded.as_str()),
        ("First request", "Latest answer")
    );
    assert!(!serde_json::to_string(&listing).unwrap().contains("TTTT"));
}

#[test]
fn opencode_files_are_private_by_a_mark_beyond_the_messages_summarized() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("opencode");
    let storage = root.join("storage");
    let info = storage.join("session/project1/ses_files.json");
    std::fs::create_dir_all(info.parent().unwrap()).unwrap();
    std::fs::write(
        &info,
        json!({"id":"ses_files","directory":"/w/q","time":{"created":1_793_000_000_000i64,"updated":1_793_000_600_000i64}}).to_string(),
    )
    .unwrap();
    for n in 0..120 {
        let message = format!("msg_{n:05}");
        let dir = storage.join("message/ses_files");
        std::fs::create_dir_all(&dir).unwrap();
        let role = if n % 2 == 0 { "user" } else { "assistant" };
        std::fs::write(
            dir.join(format!("{message}.json")),
            json!({"id":message,"role":role,"time":{"created":1_793_000_000_000i64 + n}})
                .to_string(),
        )
        .unwrap();
        let parts = storage.join(format!("part/{message}"));
        std::fs::create_dir_all(&parts).unwrap();
        let text = if n == 40 { "keep it #private" } else { "text" };
        std::fs::write(
            parts.join("prt_1.json"),
            json!({"id":"prt_1","type":"text","text":text}).to_string(),
        )
        .unwrap();
    }
    let listing = local(
        &Roots {
            opencode: Some(root),
            ..Default::default()
        },
        10,
        "m",
        &Filter::default(),
    );
    assert!(listing.sessions.is_empty());
    assert!(listing.unavailable.is_empty());
}

#[test]
fn a_mark_that_starts_a_line_is_seen_where_the_file_is_not_parsed() {
    let dir = tempfile::tempdir().unwrap();
    // A request whose mark starts its second line, in a file over 2.5 MB …
    let mut lines = vec![user("newline", "First request")];
    let filler = "x".repeat(200_000);
    for _ in 0..8 {
        lines.push(agent("newline", &filler));
    }
    lines.push(user("newline", "Fix it\n#private"));
    for _ in 0..8 {
        lines.push(agent("newline", &filler));
    }
    claude_lines(dir.path(), "newline", &lines);
    // … and in a line too long to parse.
    claude_lines(
        dir.path(),
        "newline-long",
        &[
            user("newline-long", "First"),
            user(
                "newline-long",
                &format!("{}\n#private", "y".repeat(300_000)),
            ),
            agent("newline-long", "Done"),
        ],
    );
    assert!(
        local(&claude_only(dir.path()), 10, "m", &Filter::default())
            .sessions
            .is_empty()
    );
    // An OpenCode file-store request beyond the messages summarized.
    let root = dir.path().join("opencode");
    let storage = root.join("storage");
    let info = storage.join("session/p/ses_nl.json");
    std::fs::create_dir_all(info.parent().unwrap()).unwrap();
    std::fs::write(&info, json!({"id":"ses_nl","directory":"/w/q","time":{"created":1_793_000_000_000i64,"updated":1_793_000_600_000i64}}).to_string()).unwrap();
    for n in 0..100i64 {
        let message = format!("msg_{n:05}");
        let messages = storage.join("message/ses_nl");
        std::fs::create_dir_all(&messages).unwrap();
        std::fs::write(messages.join(format!("{message}.json")), json!({"id":message,"role":if n % 2 == 0 {"user"} else {"assistant"},"time":{"created":1_793_000_000_000i64 + n}}).to_string()).unwrap();
        let parts = storage.join(format!("part/{message}"));
        std::fs::create_dir_all(&parts).unwrap();
        let text = if n == 40 { "Fix it\n#private" } else { "text" };
        std::fs::write(
            parts.join("prt_1.json"),
            json!({"id":"prt_1","type":"text","text":text}).to_string(),
        )
        .unwrap();
    }
    let listing = local(
        &Roots {
            opencode: Some(root),
            ..Default::default()
        },
        10,
        "m",
        &Filter::default(),
    );
    assert!(listing.sessions.is_empty() && listing.unavailable.is_empty());
}

#[test]
fn secret_values_in_common_shapes_are_dropped() {
    let scrubber = Scrubber::new();
    for text in [
        "**Password:**\nHunter2!",
        "The password is:\nHunter2!",
        "PGPASSWORD=Hunter2 psql",
        "pass: Hunter2",
        "key: AbCd1234",
        "mysql -u root -pHunter2",
        "curl -u admin:Hunter2 https://x", // gitleaks:allow
    ] {
        let kept = scrubber.scrub(&format!("Before.\n{text}\nAfter."));
        assert_eq!(kept, "Before. After.", "{text}");
    }
}

#[test]
fn absolute_paths_are_found_wherever_they_start_and_prose_is_kept() {
    let scrubber = Scrubber::new();
    for (text, kept) in [
        (
            "see **/home/name/project/file.rs** now",
            "see **file.rs** now",
        ),
        ("open “/home/name/x” now", "open “x” now"),
        ("error at/home/name/x", "error atx"),
        ("cc -I/home/name/include main.c", "cc -Iinclude main.c"),
        ("PATH=/home/name/bin:/home/name/.cargo/bin", "PATH=bin:bin"),
        (
            "updated /home/u/a.rs and src/b.rs",
            "updated a.rs and src/b.rs",
        ),
        (
            "edit /work/repo/main.rs and src/llm/agent.rs",
            "edit main.rs and src/llm/agent.rs",
        ),
        (
            "open \"/srv/Big Project/My Notes.md\" later",
            "open \"My Notes.md\" later",
        ),
        (
            "see https://example.com/home/x/y",
            "see https://example.com/home/x/y",
        ),
    ] {
        assert_eq!(scrubber.scrub(text), kept, "{text}");
    }
}

#[test]
fn fences_in_quotes_inline_fences_and_nested_injected_tags() {
    let scrubber = Scrubber::new();
    assert_eq!(
        scrubber.scrub("Quoted:\n> ```\n> let code = 1;\n> ```\nDone."),
        "Quoted: Done."
    );
    assert_eq!(
        scrubber.scrub("```js``` is inline\nStill prose."),
        "```js``` is inline Still prose."
    );
    assert_eq!(
        scrubber.scrub(
            "<system-reminder>a <system-reminder>b</system-reminder> TAIL</system-reminder>Real"
        ),
        "Real"
    );
}

#[test]
fn opencode_marks_hide_behind_no_other_part_and_an_unreadable_part_fails_its_session() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("opencode");
    std::fs::create_dir_all(&root).unwrap();
    let mut db = rusqlite::Connection::open(root.join("opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE session(id TEXT, parent_id TEXT, directory TEXT, time_created INTEGER, time_updated INTEGER);
         CREATE TABLE message(id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
         CREATE TABLE part(id TEXT, message_id TEXT, session_id TEXT, data TEXT);
         INSERT INTO session VALUES('ses_hidden', NULL, '/w/h', 1793000000000, 1793000600000);",
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    // 250 agent and tool parts that mention the mark, then the user's own.
    for n in 0..250 {
        tx.execute(
            "INSERT INTO message VALUES(?1, 'ses_hidden', ?2, '{\"role\":\"assistant\"}')",
            rusqlite::params![format!("a{n:04}"), 1_793_000_000_000i64 + n],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO part VALUES(?1, ?2, 'ses_hidden', ?3)",
            rusqlite::params![
                format!("pa{n:04}"),
                format!("a{n:04}"),
                json!({"type":"tool","state":{"output":"write #private to hide"}}).to_string()
            ],
        )
        .unwrap();
    }
    tx.execute(
        "INSERT INTO message VALUES('u1', 'ses_hidden', 1793000500000, '{\"role\":\"user\"}')",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO part VALUES('pu1', 'u1', 'ses_hidden', ?1)",
        [json!({"type":"text","text":"Fix it\n#private"}).to_string()],
    )
    .unwrap();
    tx.commit().unwrap();
    let listing = local(
        &Roots {
            opencode: Some(root),
            ..Default::default()
        },
        10,
        "m",
        &Filter::default(),
    );
    assert!(listing.sessions.is_empty() && listing.unavailable.is_empty());
    // A file-store part that cannot be read fails its session.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("opencode");
    opencode_session(&root, "ses_broken", 1, "Ask", "Answer");
    std::fs::write(
        root.join("storage/part/msg_ses_broken_1/prt_1.json"),
        "not json",
    )
    .unwrap();
    let listing = local(
        &Roots {
            opencode: Some(root),
            ..Default::default()
        },
        10,
        "m",
        &Filter::default(),
    );
    assert!(listing.sessions.is_empty());
    assert_eq!(listing.unavailable[0].reason, "unreadable");
}

#[test]
fn words_that_end_like_a_secret_name_are_kept_and_password_flags_are_exact() {
    let scrubber = Scrubber::new();
    for kept in [
        "Hice dos cosas: actualicé el lector y corregí las pruebas.",
        "Quedan pendientes las empresas: Acme y Globex.",
        "Las causas: un índice desactualizado y un error de permisos.",
        "The OAuth: PKCE flow stays as is.",
        "Use the -preview flag and patch -p1 to apply.",
        "Serve it with -p 8080 and mkdir -p build first.",
    ] {
        assert_eq!(scrubber.scrub(kept), kept, "{kept}");
    }
    for dropped in [
        "PGPASSWORD=Hunter2 psql",
        "DBPASS=Hunter2 ./run",
        "mysql -u root -pHunter2",
        "docker login -u name -p Hunter2",
        "sshpass -p Hunter2 ssh host",
        "pw: Hunter2",
    ] {
        assert_eq!(
            scrubber.scrub(&format!("Before.\n{dropped}\nAfter.")),
            "Before. After.",
            "{dropped}"
        );
    }
}

#[test]
fn windows_and_home_variable_paths_lose_every_directory() {
    let scrubber = Scrubber::new();
    for (text, kept) in [
        (
            r"C:\Users\name\OneDrive - Acme Corp\Documents\plan.docx",
            "plan.docx",
        ),
        ("D:/Work/Client X/Phase 2/report.xlsx", "report.xlsx"),
        ("$HOME/clients/acme/plan.md", "plan.md"),
        ("${HOME}/clients/acme/plan.md", "plan.md"),
        (r"%USERPROFILE%\clients\acme\plan.md", "plan.md"),
        ("~/clients/acme/plan.md", "plan.md"),
        (
            "updated /home/u/a.rs and src/b.rs",
            "updated a.rs and src/b.rs",
        ),
    ] {
        assert_eq!(scrubber.scrub(text), kept, "{text}");
    }
}

#[test]
fn the_final_report_of_a_continued_claude_session_is_its_last_answer_after_the_stop() {
    let root = tempfile::tempdir().unwrap();
    let id = "44444444-4444-4444-8444-444444444444";
    let path = root
        .path()
        .join(format!("projects/D--code-payments/{id}.jsonl"));
    let line = |kind: &str, message: &str, text: &str, minutes: u64| json!({"type":kind,"sessionId":id,"message":{"id":message,"role":kind,"content":[{"type":"text","text":text}],"stop_reason":(kind == "assistant").then_some("end_turn")},"timestamp":at(minutes)});
    let stopped = at(10);
    // Before the stop, then continued: a request, a tool call, and the report in two lines.
    write_lines(
        &path,
        &[
            line("user", "", "Implement it.", 0),
            line("assistant", "m1", "Working on it.", 5),
            line("user", "", "Continue and finish.", 20),
            json!({"type":"assistant","sessionId":id,"message":{"id":"m2","role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"git push"}}],"stop_reason":"tool_use"},"timestamp":at(21)}),
            json!({"type":"user","sessionId":id,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"pushed"}]},"timestamp":at(21)}),
            line(
                "assistant",
                "m3",
                "Pushed and opened the pull requests.",
                22,
            ),
            line(
                "assistant",
                "m3",
                "Branch: feature/continued\nBranch payments-web: feature/web",
                22,
            ),
        ],
    );
    let report = claude_final_answer(root.path(), id, &stopped).unwrap();
    assert_eq!(
        report.as_deref(),
        Some(
            "Pushed and opened the pull requests.\nBranch: feature/continued\nBranch payments-web: feature/web"
        )
    );
    // Nothing after the stop, a session whose last message is a request, an unknown session
    // and one marked private give none.
    assert_eq!(claude_final_answer(root.path(), id, &at(30)).unwrap(), None);
    let waiting = "55555555-5555-4555-8555-555555555555";
    write_lines(
        &root
            .path()
            .join(format!("projects/D--code-payments/{waiting}.jsonl")),
        &[
            line("assistant", "m1", "Branch: feature/early", 20),
            line("user", "", "And the tests?", 21),
        ],
    );
    assert_eq!(
        claude_final_answer(root.path(), waiting, &stopped).unwrap(),
        None
    );
    // A session still working: its last text came before a tool call, or did not end the turn.
    let working = "88888888-8888-4888-8888-888888888888";
    let working_path = root
        .path()
        .join(format!("projects/D--code-payments/{working}.jsonl"));
    write_lines(
        &working_path,
        &[
            line("user", "", "Continue and finish.", 20),
            json!({"type":"assistant","sessionId":working,"message":{"id":"m4","role":"assistant","content":[{"type":"text","text":"Now I'll run the tests."}],"stop_reason":"tool_use"},"timestamp":at(21)}),
            json!({"type":"assistant","sessionId":working,"message":{"id":"m4","role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"cargo test"}}],"stop_reason":"tool_use"},"timestamp":at(21)}),
            json!({"type":"user","sessionId":working,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]},"timestamp":at(22)}),
        ],
    );
    assert_eq!(
        claude_final_answer(root.path(), working, &stopped).unwrap(),
        None
    );
    write_lines(
        &working_path,
        &[
            line("user", "", "Continue and finish.", 20),
            json!({"type":"assistant","sessionId":working,"message":{"id":"m4","role":"assistant","content":[{"type":"text","text":"Now I'll run the tests."}],"stop_reason":null},"timestamp":at(21)}),
        ],
    );
    assert_eq!(
        claude_final_answer(root.path(), working, &stopped).unwrap(),
        None
    );
    let unknown = "66666666-6666-4666-8666-666666666666";
    assert_eq!(
        claude_final_answer(root.path(), unknown, &stopped).unwrap(),
        None
    );
    let private = "77777777-7777-4777-8777-777777777777";
    write_lines(
        &root
            .path()
            .join(format!("projects/D--code-payments/{private}.jsonl")),
        &[
            line("user", "", "#private continue", 20),
            line("assistant", "m1", "Branch: feature/secret", 21),
        ],
    );
    assert_eq!(
        claude_final_answer(root.path(), private, &stopped).unwrap(),
        None
    );
}

/// OpenCode's second database layout, as its newer versions write it: sessions in
/// `session_v2`, each message with its content in `session_message`, `session_part` alongside,
/// and other tables (accounts, credentials) that are never read.
fn second_layout(root: &Path) -> rusqlite::Connection {
    std::fs::create_dir_all(root).unwrap();
    let db = rusqlite::Connection::open(root.join("opencode.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE session_v2(id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT, slug TEXT NOT NULL, directory TEXT NOT NULL, title TEXT, version TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, time_archived INTEGER);
         CREATE TABLE session_message(id TEXT PRIMARY KEY, session_id TEXT NOT NULL, type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
         CREATE TABLE session_part(message_id TEXT NOT NULL, session_id TEXT NOT NULL, ordinal INTEGER NOT NULL, type TEXT NOT NULL, call_id TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
         CREATE TABLE credential(id TEXT, value TEXT);
         INSERT INTO credential VALUES('c', 'oc_live_SYNTHETICCREDENTIAL9f8e7d6c5b4a'); -- gitleaks:allow",
    )
    .unwrap();
    db
}
fn second_message(db: &rusqlite::Connection, session: &str, seq: i64, kind: &str, data: Value) {
    db.execute(
        "INSERT INTO session_message VALUES(?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        rusqlite::params![
            format!("{session}-m{seq}"),
            session,
            kind,
            seq,
            1_793_000_000_000i64 + seq * 1000,
            data.to_string()
        ],
    )
    .unwrap();
}

#[test]
fn the_second_opencode_layout_is_read_from_its_messages_without_reasoning_or_tools() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("opencode");
    let db = second_layout(&root);
    db.execute_batch(
        "INSERT INTO session_v2 VALUES('ses_a', 'p1', NULL, 'a', '/home/user/src/gateway', 'Gateway', '1', 1793000000000, 1793000600000, NULL);
         INSERT INTO session_v2 VALUES('ses_child', 'p1', 'ses_a', 'b', '/home/user/src/gateway', 'Sub', '1', 1793000000000, 1793000900000, NULL);
         INSERT INTO session_v2 VALUES('ses_private', 'p1', NULL, 'c', '/home/user/src/gateway', 'Mine', '1', 1793000000000, 1793000700000, NULL);
         INSERT INTO session_v2 VALUES('ses_parts', 'p1', NULL, 'd', '/home/user/src/queue', 'Queue', '1', 1793000000000, 1793000500000, NULL);",
    )
    .unwrap();
    second_message(
        &db,
        "ses_a",
        1,
        "system",
        json!({"text":"SYSTEM NOTICE","time":{"created":1}}),
    );
    second_message(
        &db,
        "ses_a",
        2,
        "user",
        json!({"text":"Why does the gateway return 502?","files":[],"agents":[],"time":{"created":1}}),
    );
    second_message(
        &db,
        "ses_a",
        3,
        "assistant",
        json!({"content":[
        {"type":"reasoning","text":"REASONING SHOULD NEVER APPEAR"},
        {"type":"tool","id":"t1","name":"bash","state":{"output":"TOOL OUTPUT SHOULD NEVER APPEAR"}},
        {"type":"text","text":"The upstream pool is exhausted; raising it is still pending."}],"time":{"created":2}}),
    );
    second_message(
        &db,
        "ses_a",
        4,
        "idle",
        json!({"outcome":"done","time":{"created":3}}),
    );
    second_message(
        &db,
        "ses_child",
        1,
        "user",
        json!({"text":"Sub-agent request"}),
    );
    second_message(
        &db,
        "ses_private",
        1,
        "user",
        json!({"text":"Look at my notes"}),
    );
    second_message(
        &db,
        "ses_private",
        2,
        "assistant",
        json!({"content":[{"type":"text","text":"Done."}]}),
    );
    second_message(
        &db,
        "ses_private",
        3,
        "user",
        json!({"text":"#private and these too"}),
    );
    // A message that keeps its text in parts.
    second_message(&db, "ses_parts", 1, "user", json!({"time":{"created":1}}));
    second_message(
        &db,
        "ses_parts",
        2,
        "assistant",
        json!({"time":{"created":2}}),
    );
    for (message, ordinal, kind, data) in [
        (
            "ses_parts-m1",
            0,
            "text",
            json!({"type":"text","text":"Drain the dead letter queue"}),
        ),
        (
            "ses_parts-m2",
            0,
            "tool",
            json!({"type":"tool","state":{"output":"PART TOOL OUTPUT"}}),
        ),
        (
            "ses_parts-m2",
            1,
            "text",
            json!({"type":"text","text":"Drained 40 messages."}),
        ),
        (
            "ses_parts-m2",
            2,
            "text",
            json!({"type":"text","text":"SYNTHETIC PART","synthetic":true}),
        ),
    ] {
        db.execute(
            "INSERT INTO session_part VALUES(?1, 'ses_parts', ?2, ?3, NULL, 1, 1, ?4)",
            rusqlite::params![message, ordinal, kind, data.to_string()],
        )
        .unwrap();
    }
    drop(db);
    let listing = local(
        &Roots {
            opencode: Some(root),
            ..Default::default()
        },
        10,
        "server",
        &Filter::default(),
    );
    assert!(listing.unavailable.is_empty(), "{:?}", listing.unavailable);
    let ids: Vec<_> = listing
        .sessions
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert_eq!(ids, ["ses_a", "ses_parts"]);
    let session = &listing.sessions[0];
    assert_eq!(session.project, "gateway");
    assert_eq!(session.asked, "Why does the gateway return 502?");
    assert_eq!(
        session.concluded,
        "The upstream pool is exhausted; raising it is still pending."
    );
    assert_eq!(session.updated_at, from_millis(1_793_000_600_000).unwrap());
    assert_eq!(listing.sessions[1].asked, "Drain the dead letter queue");
    assert_eq!(listing.sessions[1].concluded, "Drained 40 messages.");
    let text = serde_json::to_string(&listing).unwrap();
    for never in [
        "REASONING",
        "TOOL OUTPUT",
        "SYSTEM NOTICE",
        "SYNTHETIC PART",
        "oc_live",
        "Sub-agent",
    ] {
        assert!(!text.contains(never), "{never} in {text}");
    }
}

#[test]
fn an_unknown_second_layout_is_unreadable_not_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("opencode");
    std::fs::create_dir_all(&root).unwrap();
    // `session_v2` without the messages' known columns, and no first layout to fall back to.
    rusqlite::Connection::open(root.join("opencode.db"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE session_v2(id TEXT, directory TEXT, time_created INTEGER, time_updated INTEGER);
             CREATE TABLE session_message(id TEXT, session_id TEXT, body TEXT);
             INSERT INTO session_v2 VALUES('ses_a', '/home/user/x', 1, 2);",
        )
        .unwrap();
    let listing = local(
        &Roots {
            opencode: Some(root),
            ..Default::default()
        },
        10,
        "server",
        &Filter::default(),
    );
    assert!(listing.sessions.is_empty());
    assert_eq!(listing.unavailable.len(), 1);
    assert_eq!(listing.unavailable[0].reason, "unreadable");
}

#[test]
fn a_project_is_its_repository_whatever_worktree_the_session_ran_in() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let src = home.join("src");
    std::fs::create_dir_all(src.join("hobby-game/.git")).unwrap();
    std::fs::create_dir_all(src.join("journal/.git/worktrees/wt")).unwrap();
    // A linked worktree outside the repository.
    let linked = home.join("worktrees/journal-wt");
    std::fs::create_dir_all(linked.join("sub")).unwrap();
    std::fs::write(
        linked.join(".git"),
        format!(
            "gitdir: {}\n",
            src.join("journal/.git/worktrees/wt").display()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(src.join("drafts/sketchbook")).unwrap();
    let of = |cwd: PathBuf| Project::of(&cwd.to_string_lossy(), Some(&home));
    // A Claude Code worktree, even one already removed, is its repository.
    let game = of(src.join("hobby-game/.claude/worktrees/feature-x"));
    assert_eq!(game.name, "hobby-game");
    assert_eq!(of(src.join("hobby-game/assets/levels")).name, "hobby-game");
    assert_eq!(of(linked.join("sub")).name, "journal");
    let plain = of(src.join("drafts/sketchbook"));
    assert_eq!(plain.name, "sketchbook");
    let at_src = of(src.clone());
    assert_eq!(at_src.name, "src");
    let excluding = |entries: &[&str]| Filter {
        exclude_projects: entries.iter().map(|e| e.to_string()).collect(),
        only_projects: vec![],
    };
    for entry in [
        "hobby-game",
        "Hobby-game",
        "~/src/hobby-game/",
        "src/hobby-game",
        "src",
    ] {
        assert!(!excluding(&[entry]).admits(&game), "{entry}");
    }
    for entry in ["drafts", "drafts/sketchbook", "sketchbook", "~/src/drafts"] {
        assert!(!excluding(&[entry]).admits(&plain), "{entry}");
    }
    // An entry names whole directories, not a part of one.
    for entry in ["hobby", "game", "src/journal", "drafts/other"] {
        assert!(excluding(&[entry]).admits(&game), "{entry}");
    }
    let only = Filter {
        exclude_projects: vec![],
        only_projects: vec!["journal".into()],
    };
    assert!(only.admits(&of(linked.clone())));
    assert!(!only.admits(&game));
    assert!(
        !only.admits(&Project::of("", Some(&home))),
        "unknown project"
    );
    assert!(Filter::default().admits(&Project::of("", Some(&home))));
    // Entries are names or directories under the home, bounded.
    for invalid in [
        "",
        "  ",
        "../etc",
        "a/./b",
        "/home/user/src/hobby-game",
        r"C:\Users\user\src\hobby-game",
        "$HOME/src",
        &"x".repeat(121),
    ] {
        assert!(excluding(&[invalid]).validate().is_err(), "{invalid:?}");
    }
    assert!(
        excluding(&["hobby-game", "drafts/sketchbook"])
            .validate()
            .is_ok()
    );
    assert!(excluding(&["p"; 101]).validate().is_err());
}

#[test]
fn an_excluded_project_never_leaves_its_machine() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join("src/hobby-game/.git")).unwrap();
    std::fs::create_dir_all(home.join("src/payments/.git")).unwrap();
    let claude = dir.path().join("claude");
    for (id, cwd, minutes) in [
        ("kept", home.join("src/payments"), 1),
        (
            "excluded",
            home.join("src/hobby-game/.claude/worktrees/bridge-x"),
            2,
        ),
    ] {
        let path = claude.join(format!("projects/p/{id}.jsonl"));
        write_lines(
            &path,
            &[
                json!({"type":"user","cwd":cwd,"message":{"role":"user","content":format!("Publish the {id} build")},"timestamp":at(minutes)}),
                json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Published."}]},"timestamp":at(minutes)}),
            ],
        );
        touch(&path, minutes);
    }
    let roots = Roots {
        home: Some(home),
        claude: Some(claude),
        ..Default::default()
    };
    let filter = Filter {
        exclude_projects: vec!["hobby-game".into()],
        only_projects: vec![],
    };
    let listing = local(&roots, 10, "laptop", &filter);
    assert!(listing.filtered);
    let ids: Vec<_> = listing
        .sessions
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert_eq!(ids, ["kept"]);
    assert_eq!(listing.sessions[0].project, "payments");
    assert!(
        !serde_json::to_string(&listing)
            .unwrap()
            .contains("excluded")
    );
    // Without the filter both are read, and the worktree's session names its repository.
    let all = local(&roots, 10, "laptop", &Filter::default());
    assert!(!all.filtered);
    assert_eq!(all.sessions[0].project, "hobby-game");
    // A listing gathered without the filter it needed is refused; one with it is checked again
    // by the project names it carries.
    assert!(all.clone().admitted(&filter).is_err());
    assert_eq!(
        all.clone()
            .admitted(&Filter::default())
            .unwrap()
            .sessions
            .len(),
        2
    );
    let mut marked = all;
    marked.filtered = true;
    let admitted = marked.admitted(&filter).unwrap();
    assert_eq!(admitted.sessions.len(), 1);
    assert_eq!(admitted.sessions[0].session_id, "kept");
}

#[test]
fn work_items_and_who_did_what_never_reach_a_summary() {
    let scrubber = Scrubber::new();
    let text = "Revisé el sprint del equipo.\n\
        #4521 — Corregir el login de pagos\n\
        Task 4522 asignada a Ana Pérez: actualizar la API\n\
        | ID | Título | Responsable |\n\
        | 4523 | Migrar reportes | Luis |\n\
        Ver https://dev.azure.com/org/proj/_workitems/edit/4524\n\
        El PR !87 sigue abierto\n\
        HU-1234 lista para QA\n\
        Creado por @Luis ayer\n\
        La migración de la base terminó y faltan las pruebas de carga.";
    let kept = scrubber.scrub(text);
    assert_eq!(
        kept,
        "Revisé el sprint del equipo. La migración de la base terminó y faltan las pruebas de carga."
    );
    for line in [
        "Fix #12",
        "PBI 345",
        "pull request 77",
        "tarea nº 88",
        "see /pull/9",
        "Fixes AB#12",
        "PR at https://dev.azure.com/org/proj/_git/repo/pullrequest/12",
    ] {
        assert!(scrubber.names_work(line), "{line}");
        assert!(scrubber.scrub(line).is_empty(), "{line}");
    }
    assert!(scrubber.scrub("Author: Ana").is_empty());
    for line in [
        "Phase 2 is done",
        "port 1433 at the firewall",
        "a 3.2 release",
        "user@example.com",
    ] {
        assert!(!scrubber.names_work(line), "{line}");
    }
}

#[test]
fn names_of_people_are_taken_out_and_found_by_their_capitalized_words() {
    let names = Names::new(["Ana María Pérez", "Luis de la Mesa", "Bo"]);
    assert_eq!(
        names.strip("Ana María Pérez pidió el informe; Luis lo revisa con Perez."),
        "someone pidió el informe; someone lo revisa con someone."
    );
    // Lowercase words and particles are not names; nor are short ones.
    assert_eq!(
        names.strip("Lo dejé sobre la mesa de Bo y de la ana"),
        "Lo dejé sobre la mesa de Bo y de la ana"
    );
    assert_eq!(
        names.found("Le pregunté a Mesa y a María"),
        ["Mesa", "María"]
    );
    assert!(names.found("Ya lo estoy revisando").is_empty());
    let without = names.clone().without(["Luis"]);
    assert!(without.found("Hola Luis").is_empty());
    assert_eq!(without.found("Hola Luis, Ana lo vio"), ["Ana"]);
    let summary = Summary {
        asked: "Ask Ana about the release".into(),
        concluded: "Pérez approved it".into(),
        ..Default::default()
    }
    .without_names(&names);
    assert_eq!(summary.asked, "Ask someone about the release");
    assert_eq!(summary.concluded, "someone approved it");
}

#[test]
fn an_agent_sessions_source_without_filters_keeps_the_shape_older_hosts_read() {
    // The variant as hosts before project filters declared it: unknown fields refused.
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
    enum OlderSpec {
        AgentSessions {
            #[allow(dead_code)]
            sessions: usize,
        },
    }
    let spec = crate::tools::ToolSpec::AgentSessions {
        sessions: 10,
        exclude_projects: vec![],
        only_projects: vec![],
    };
    let written = serde_json::to_value(&spec).unwrap();
    assert_eq!(written, json!({"type":"agent_sessions","sessions":10}));
    assert!(serde_json::from_value::<OlderSpec>(written).is_ok());
    // A listing without a filter keeps its older shape too.
    let listing = serde_json::to_value(Listing::default()).unwrap();
    assert_eq!(listing, json!({"sessions":[],"unavailable":[]}));
    // With filters the fields are written and read back.
    let filtered = crate::tools::ToolSpec::AgentSessions {
        sessions: 5,
        exclude_projects: vec!["hobby-game".into()],
        only_projects: vec![],
    };
    let text = serde_json::to_string(&filtered).unwrap();
    assert_eq!(
        text,
        r#"{"type":"agent_sessions","sessions":5,"exclude_projects":["hobby-game"]}"#
    );
    let back: crate::tools::ToolSpec = serde_json::from_str(&text).unwrap();
    assert_eq!(
        back.session_filter().unwrap().exclude_projects,
        ["hobby-game"]
    );
}

#[test]
fn a_home_that_is_a_repository_bare_repositories_and_relative_worktrees_name_their_project() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    // A home kept as a dotfiles repository is not every directory's project.
    std::fs::create_dir_all(home.join(".git")).unwrap();
    std::fs::create_dir_all(home.join("src/drafts/sketchbook")).unwrap();
    let plain = Project::of(
        &home.join("src/drafts/sketchbook").to_string_lossy(),
        Some(&home),
    );
    assert_eq!(plain.name, "sketchbook");
    let excluding = |entry: &str| Filter {
        exclude_projects: vec![entry.into()],
        only_projects: vec![],
    };
    assert!(!excluding("drafts").admits(&plain));
    // A worktree of a bare repository (`commondir` names it), and one whose `.git` file points
    // with a relative path.
    let bare = home.join("repos/hobby-game.git");
    std::fs::create_dir_all(bare.join("worktrees/wt")).unwrap();
    std::fs::write(bare.join("worktrees/wt/commondir"), "../..\n").unwrap();
    let worktree = home.join("work/wt");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", bare.join("worktrees/wt").display()),
    )
    .unwrap();
    let in_bare = Project::of(&worktree.to_string_lossy(), Some(&home));
    assert_eq!(in_bare.name, "hobby-game");
    assert!(!excluding("hobby-game").admits(&in_bare));
    std::fs::create_dir_all(home.join("src/journal/.git/worktrees/rel")).unwrap();
    let relative = home.join("src/journal-rel");
    std::fs::create_dir_all(&relative).unwrap();
    std::fs::write(
        relative.join(".git"),
        "gitdir: ../journal/.git/worktrees/rel\n",
    )
    .unwrap();
    let in_relative = Project::of(&relative.to_string_lossy(), Some(&home));
    assert_eq!(in_relative.name, "journal");
    assert!(!excluding("journal").admits(&in_relative));
}

#[test]
fn the_second_opencode_layout_reads_plain_content_and_refuses_unknown_message_types() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("opencode");
    let db = second_layout(&root);
    db.execute_batch(
        "INSERT INTO session_v2 VALUES('ses_s', 'p1', NULL, 'a', '/home/user/src/gateway', 'G', '1', 1793000000000, 1793000600000, NULL);",
    )
    .unwrap();
    second_message(
        &db,
        "ses_s",
        1,
        "user",
        json!({"text":"Restart the gateway"}),
    );
    second_message(
        &db,
        "ses_s",
        2,
        "assistant",
        json!({"content":"Restarted."}),
    );
    second_message(&db, "ses_s", 3, "user", json!({"text":"And the cache?"}));
    second_message(
        &db,
        "ses_s",
        4,
        "assistant",
        json!({"content":["loose", {"type":"text","text":"Cleared too."}]}),
    );
    drop(db);
    let roots = Roots {
        opencode: Some(root.clone()),
        ..Default::default()
    };
    let listing = local(&roots, 10, "server", &Filter::default());
    assert!(listing.unavailable.is_empty(), "{:?}", listing.unavailable);
    assert_eq!(listing.sessions[0].concluded, "Cleared too.");
    // Messages of types Kuroko does not know are another layout, not sessions without requests.
    let db = rusqlite::Connection::open(root.join("opencode.db")).unwrap();
    db.execute_batch("UPDATE session_message SET type = 'turn';")
        .unwrap();
    drop(db);
    let listing = local(&roots, 10, "server", &Filter::default());
    assert!(listing.sessions.is_empty());
    assert_eq!(listing.unavailable[0].reason, "unreadable");
}

#[test]
fn kurokos_own_reading_of_its_runs_keeps_work_item_lines() {
    let dir = tempfile::tempdir().unwrap();
    let roots = claude_only(dir.path());
    let path = claude_lines(
        dir.path(),
        "run",
        &[
            user("run", "Implement the export"),
            agent("run", "PR #12 opened.\nThe export works."),
        ],
    );
    touch(&path, 2);
    let source = local(&roots, 10, "m", &Filter::default());
    assert_eq!(source.sessions[0].concluded, "The export works.");
    let own = local_reading(&roots, 10, "m", &Filter::default(), true);
    assert_eq!(
        own.sessions[0].concluded,
        "PR #12 opened. The export works."
    );
}
