//! The factory rehearsed end to end with synthetic providers only (epic #238, #247): the engine's
//! synthetic Azure DevOps catalog (it has no way to write and records every read) and fake
//! provider CLI, a mocked Microsoft Graph (the user's chats and their members, and every POST
//! counted) and a launcher that is a fake program. An HU enters a configured ready state, the
//! watcher starts a run, the director chooses among two workflows that start by themselves, the
//! plan is reviewed and confirmed (the gate), two children are coded one after the other in their
//! repositories' checkouts, their pull requests are found and completed, the launcher runs, the
//! team is told with a mention, and a summary is written and sent to a group chat; the host is
//! restarted three times on the way. A task then goes through the direct coding path. Every write
//! to a provider is counted: the children only through the user's confirmation, one post per
//! notification and one per send. A second test drives the same operations through the tool layer
//! that `kuroko tools call` and `kuroko mcp` share, with the confirmations the catalog declares.
//! A third rehearses a workflow lead's whole shift out of shadow: it moves the item, creates the
//! pull request's support task, closes the run and writes its record, each write counted once.
use super::record::{Run, State as Phase};
use super::runs::tests::{Bench, HU, TASK};
use super::*;
use crate::store::settings::SettingsStore;
use crate::workflows::director::ReadyStates;
use crate::workflows::{Block, Chat, Kind, LauncherRef, Stage};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// The team's group chat: the done notification and the summary go there.
const TEAM: &str = "19:team@thread.v2";
const API: &str = "0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f";
const WEB: &str = "1a2b3c4d-5b1d-4f7a-9c8e-0f3c6a2e5e6f";

struct Token;
#[async_trait::async_trait]
impl crate::adapters::oauth::AccessToken for Token {
    async fn access_token(&self) -> anyhow::Result<String> {
        Ok("synthetic-graph-token".into())
    }
}

/// A mocked Graph: `me`, the user's chats (the team group with Victoria and Ana), the team's
/// members, and every POST answered as created and counted.
struct Teams {
    server: MockServer,
    dir: tempfile::TempDir,
    graph: Arc<crate::adapters::graph::Graph>,
}
impl Teams {
    async fn new(host: &Arc<Host>) -> Self {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let config = host.state.settings.config().unwrap();
        let me = config.graph.user_id.clone();
        let members = json!([
            {"userId": me, "displayName": "Yo"},
            {"userId": "vic-id", "displayName": "Victoria Gómez"},
            {"userId": "ana-id", "displayName": "Ana Pérez"},
        ]);
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": me})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/me/chats"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
                {"id": TEAM, "topic": "Equipo pagos", "chatType": "group", "members": members,
                 "lastUpdatedDateTime": "2026-11-01T10:00:00Z"},
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/chats/{TEAM}/members")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": members})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "message-1"})))
            .mount(&server)
            .await;
        let graph = Arc::new(crate::adapters::graph::Graph {
            client: reqwest::Client::new(),
            token: Arc::new(Token),
            base_url: server.uri(),
            config: Arc::new(config),
            store: Arc::new(crate::state::Store::open(&dir.path().join("graph.db")).unwrap()),
            client_state: "synthetic-client-state-of-32-chars".into(),
            config_path: None,
        });
        let teams = Self { server, dir, graph };
        teams.attach(host).await;
        teams
    }
    /// The Microsoft session the running assistant holds (again after a restart).
    async fn attach(&self, host: &Arc<Host>) {
        *host.state.activity_graph.lock().await = Some(self.graph.clone());
    }
    /// Every POST, as `(path, body)`, oldest first.
    async fn posts(&self) -> Vec<(String, String)> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .map(|r| {
                (
                    r.url.path().to_owned(),
                    String::from_utf8_lossy(&r.body).into_owned(),
                )
            })
            .collect()
    }
    fn events(&self, kind: &str) -> Vec<String> {
        crate::state::Store::inspect(&self.dir.path().join("graph.db"), true, 100, None, false)
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == kind)
            .map(|e| e["detail"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// A launcher of the coordinator that appends a line to `marker` each time it runs (a release in
/// a test environment); approved with the digest of its program.
fn launcher(bench: &Bench, marker: &std::path::Path) -> crate::consumers::Launcher {
    use std::os::unix::fs::PermissionsExt;
    let program = bench.root.path().join("release.sh");
    fs::write(
        &program,
        format!("#!/bin/sh\necho run >> '{}'\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let program = program.canonicalize().unwrap();
    let digest = crate::hash::sha256_hex(fs::read(&program).unwrap());
    crate::consumers::Launcher {
        id: "release-desa".into(),
        description: "Synthetic release in desa".into(),
        program,
        digest,
        args: Vec::new(),
        directory: bench.api.clone(),
        env: BTreeMap::new(),
        timeout_seconds: 30,
        created_at: 0,
        approved_at: Some(1),
    }
}

/// The factory as the person sets it up: the workflow's blocks (its instruction, a note to the
/// personal chat when the plan is ready, the approved launcher once the pull requests are
/// completed, and the team told with a mention when the run is done), a second workflow of the
/// project that also starts by itself (so the director chooses), the director, and a ready state.
fn set_up(bench: &Bench, release: &crate::consumers::Launcher) {
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflow_blocks.extend([
        Block {
            id: "plan-note".into(),
            name: "Tell me the plan is ready".into(),
            kind: Kind::TeamsNotification,
            stage: Some(Stage::PlanReady),
            chat: Some(Chat {
                id: "self".into(),
                name: "Personal chat".into(),
            }),
            template: "Plan ready for {item}: {link}".into(),
            ..Default::default()
        },
        Block {
            id: "release".into(),
            name: "Release in desa".into(),
            kind: Kind::Launcher,
            stage: Some(Stage::PullRequestsCompleted),
            launcher: Some(LauncherRef {
                id: release.id.clone(),
                digest: release.digest.clone(),
            }),
            ..Default::default()
        },
        Block {
            id: "tell-victoria".into(),
            name: "Tell Victoria the release is ready".into(),
            kind: Kind::TeamsNotification,
            stage: Some(Stage::Done),
            chat: Some(Chat {
                id: TEAM.into(),
                name: "Equipo pagos".into(),
            }),
            mentions: vec!["Victoria Gómez".into()],
            template: "The release of {item} is ready in desa: {pull_requests}".into(),
            ..Default::default()
        },
    ]);
    let payments = &mut map.workflows[0];
    payments.description = "Features and bugs of the payments API and web.".into();
    payments.auto_start = true;
    payments.blocks = ["base", "plan-note", "release", "tell-victoria"]
        .map(String::from)
        .to_vec();
    let mut docs = map.workflows[0].clone();
    docs.id = "docs".into();
    docs.name = "Docs".into();
    docs.description = "Documentation only.".into();
    docs.blocks = vec!["base".into()];
    map.workflows.push(docs);
    map.workflow_settings.ready_states = vec![ReadyStates {
        organization: super::synthetic::ORGANIZATION.into(),
        project: "Payments".into(),
        states: vec!["Ready for development".into()],
    }];
    bench.host.state.settings.save_map(&map).unwrap();
    // The director (its fake CLI answers «payments»).
    bench.director();
}

/// Restart the host on the same profile; the running assistant's Microsoft session comes back.
async fn restart(bench: &mut Bench, teams: &Teams) {
    bench.restart().await;
    teams.attach(&bench.host).await;
}

/// The control dispatcher as the CLI's local channel reaches it (also what the tool layer uses).
async fn call(host: &Arc<Host>, name: &str, args: Value) -> control::Reply {
    let mut request = control::Request::new(name);
    request.args = args;
    authority::within(
        authority::Invocation {
            scope: host.state.authority.clone(),
            transport: authority::Transport::Local,
        },
        control::dispatch(host, request),
    )
    .await
}

/// The director's fake CLI answers the summary task with `text` (planning and choosing are over).
fn summary_answer(bench: &Bench, text: &str) {
    let answer = json!({"summary": text});
    let line = json!({"type":"text","part":{"text":answer.to_string()}});
    for file in ["director.jsonl", "plan.jsonl"] {
        fs::write(bench.log.join(file), format!("{line}\n")).unwrap();
    }
}

fn stages(run: &Run) -> Vec<&'static str> {
    run.timeline.iter().map(|s| s.stage.word()).collect()
}
fn ran(marker: &std::path::Path) -> usize {
    fs::read_to_string(marker)
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

#[tokio::test]
async fn the_factory_takes_an_hu_from_its_ready_state_to_the_release_and_a_summary_in_teams() {
    let mut bench = Bench::new().await;
    let teams = Teams::new(&bench.host).await;
    let marker: PathBuf = bench.root.path().join("release.ran");
    let release = launcher(&bench, &marker);
    consumers::add_test_launcher(&bench.host.state, release.clone());
    set_up(&bench, &release);
    for id in [HU, TASK, 101, 102] {
        bench.catalog.set_state(id, "New");
    }

    // Nothing is ready: the watcher reads and starts nothing.
    let quiet = watcher::poll(&bench.host, &bench.azure, bench.now()).await;
    assert_eq!(quiet["started"], 0, "{quiet}");
    assert!(
        bench
            .host
            .state
            .workflows
            .db
            .list(&[], None, -1)
            .unwrap()
            .is_empty()
    );

    // The HU reaches the ready state: the watcher starts one run, the director chooses.
    bench.catalog.set_state(HU, "Ready for development");
    let poll = watcher::poll(&bench.host, &bench.azure, bench.now()).await;
    assert_eq!(poll["started"], 1, "{poll}");
    let runs = bench.host.state.workflows.db.list(&[], None, -1).unwrap();
    assert_eq!(runs.len(), 1);
    let id = runs[0].id.clone();
    assert!(runs[0].auto_start);
    // The next poll sees the item again and starts nothing more.
    let again = watcher::poll(&bench.host, &bench.azure, bench.now() + 3600).await;
    assert_eq!(again["started"], 0, "{again}");

    // The director chose «payments»; it plans with the workflow's instructions and blocks and
    // stops at the gate. The personal-chat note is posted once.
    let run = bench.state(&id, Phase::PlanReview).await;
    assert_eq!(run.workflow.as_deref(), Some("payments"));
    assert_eq!(run.classification.as_ref().unwrap()["outcome"], "chosen");
    assert_eq!(bench.count("directions"), 1);
    assert_eq!(bench.count("plans"), 1);
    let posts = teams.posts().await;
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/chats/48:notes/messages");
    assert!(posts[0].1.contains("Plan ready for") && posts[0].1.contains("/_workitems/edit/100"));
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(bench.run(&id).state, Phase::PlanReview);
    assert_eq!(bench.count("starts"), 0);

    // Restart 1: at the gate. Nothing is posted or planned again.
    restart(&mut bench, &teams).await;
    for _ in 0..3 {
        bench.tick().await;
    }
    let run = bench.run(&id);
    assert_eq!(run.state, Phase::PlanReview);
    assert_eq!((bench.count("plans"), teams.posts().await.len()), (1, 1));

    // The person reviews and confirms the children: the one creation in the ledger.
    bench.confirm(&run, "created", &[101, 102]).await;

    // Restart 2: right after the confirmation, before the engine saw it.
    restart(&mut bench, &teams).await;

    // Two children coded one after the other, each in its repository's checkout.
    let run = bench.state(&id, Phase::PullRequests).await;
    assert_eq!(bench.count("starts"), 2);
    let (first, second) = (&run.children[0], &run.children[1]);
    assert_eq!((first.id, second.id), (101, 102));
    assert!(second.started_at.unwrap() >= first.finished_at.unwrap());
    assert_eq!(first.directory.as_deref(), Some(bench.web.as_path()));
    assert_eq!(second.directory.as_deref(), Some(bench.api.as_path()));

    // Their pull requests are found by branch; the run waits while they are open.
    bench.pulls(WEB, "payments-web", "feature/child-1", 11, "active");
    bench.pulls(API, "payments-api", "feature/child-2", 12, "active");
    bench
        .until(&id, |r| {
            r.children.iter().all(|c| !c.pull_requests.is_empty())
        })
        .await;

    // Restart 3: while waiting for the pull requests.
    restart(&mut bench, &teams).await;
    bench.tick().await;
    assert_eq!(bench.run(&id).state, Phase::PullRequests);
    assert_eq!(ran(&marker), 0);

    // Completed: the launcher runs, the team is told with the mention, the run is done.
    bench.pulls(WEB, "payments-web", "feature/child-1", 11, "completed");
    bench.pulls(API, "payments-api", "feature/child-2", 12, "completed");
    let run = bench.state(&id, Phase::Done).await;
    assert_eq!(
        stages(&run),
        [
            "classifying",
            "planning",
            "plan_review",
            "creating_children",
            "coding",
            "pull_requests",
            "blocks",
            "done"
        ]
    );
    assert_eq!(ran(&marker), 1);
    let posts = teams.posts().await;
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert_eq!(posts[1].0, format!("/chats/{TEAM}/messages"));
    assert!(posts[1].1.contains("vic-id"), "the mention: {}", posts[1].1);
    assert!(posts[1].1.contains("pullrequest/11") && posts[1].1.contains("pullrequest/12"));
    let outcomes: Vec<(&str, &str)> = run
        .block_outcomes
        .iter()
        .map(|o| (o.block.as_str(), o.outcome.as_str()))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("plan-note", "sent"),
            ("release", "succeeded"),
            ("tell-victoria", "sent")
        ]
    );

    // A summary by the director, from the record, sent to the team with a mention.
    assert_eq!(bench.count("plans"), 1);
    summary_answer(
        &bench,
        "La HU **Pay by card** está lista: dos hijos con sus pull requests completados y el release en desa.",
    );
    let written = call(&bench.host, "workflow_summary", json!({"run": id})).await;
    assert!(written.ok, "{:?}", written.message);
    let summary = written.data["summary"]["id"].as_str().unwrap().to_owned();
    let unconfirmed = call(
        &bench.host,
        "workflow_summary_send",
        json!({"run": id, "summary": summary, "chat": TEAM, "mentions": ["Victoria Gómez"]}),
    )
    .await;
    assert!(!unconfirmed.ok && unconfirmed.message.unwrap().contains("Pay by card"));
    assert_eq!(teams.posts().await.len(), 2);
    let sent = call(
        &bench.host,
        "workflow_summary_send",
        json!({"run": id, "summary": summary, "chat": TEAM, "mentions": ["Victoria Gómez"], "confirmed": true}),
    )
    .await;
    assert!(sent.ok, "{:?}", sent.message);
    let posts = teams.posts().await;
    assert_eq!(posts.len(), 3);
    assert!(posts[2].1.contains("Pay by card") && posts[2].1.contains("vic-id"));
    assert_eq!(bench.run(&id).summary_sends.len(), 1);

    // The task goes the direct way: no plan, no children; coded, its pull request completed,
    // the launcher and the done notification once more.
    let task = bench.start(TASK).await;
    bench
        .until(&task, |r| {
            r.children.first().is_some_and(|c| c.branch.is_some())
        })
        .await;
    bench.pulls(WEB, "payments-web", "feature/child-3", 13, "completed");
    let done = bench.state(&task, Phase::Done).await;
    assert_eq!(done.children.len(), 1);
    assert_eq!(done.children[0].id, TASK);
    assert!(!stages(&done).contains(&"plan_review"));
    // One planning session and the summary's (the fake CLI counts every closed-JSON session that
    // is not a classification as a plan); three coding starts: two children and the task.
    assert_eq!((bench.count("plans"), bench.count("starts")), (2, 3));
    assert_eq!(ran(&marker), 2);

    // Every write, counted: the one creation the person confirmed, one post per notification
    // (the plan note, the HU's and the task's done) and one per send; Azure was only read.
    assert_eq!(bench.ledger_entries(), 1);
    assert_eq!(teams.posts().await.len(), 4);
    assert!(bench.catalog.only_reads());
    assert_eq!(teams.events("workflow_notification").len(), 3);
}

#[tokio::test]
async fn the_cli_and_mcp_tool_layer_drive_the_runs_with_the_confirmations_the_catalog_declares() {
    let bench = Bench::new().await;
    let teams = Teams::new(&bench.host).await;
    bench.director();
    let id = bench.start(HU).await;
    bench.state(&id, Phase::PlanReview).await;
    let host = bench.host.clone();
    let dispatch: crate::app::tools::Dispatch = Arc::new(move |request| {
        let host = host.clone();
        Box::pin(async move {
            Ok(authority::within(
                authority::Invocation {
                    scope: host.state.authority.clone(),
                    transport: authority::Transport::Local,
                },
                control::dispatch(&host, request),
            )
            .await)
        })
    });
    let tool = |name: &'static str, args: Value| {
        let dispatch = dispatch.clone();
        async move { crate::app::tools::run_with(dispatch, name, &args).await }
    };

    // What the catalog declares: reads, local writes, and the operations that need the user.
    for (name, effect) in [
        ("workflow_runs", "read"),
        ("workflow_run_status", "read"),
        ("workflow_summary", "local"),
        ("workflow_run_start", "needs confirmation"),
        ("workflow_run_resume", "needs confirmation"),
        ("workflow_run_cancel", "needs confirmation"),
        ("workflow_run_assign", "needs confirmation"),
        ("workflow_summary_send", "needs confirmation"),
    ] {
        let catalog = crate::app::tools::catalog();
        let entry = catalog
            .as_array()
            .or_else(|| catalog["tools"].as_array())
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is in the catalog"))
            .clone();
        assert_eq!(entry["effect"], effect, "{name}");
    }

    // Reads through the tool layer.
    let listed = tool("workflow_runs", json!({})).await.unwrap();
    assert_eq!(listed["runs"][0]["id"], id.as_str());
    let shown = tool("workflow_run_status", json!({"run": id}))
        .await
        .unwrap();
    assert_eq!(shown["run"]["state"], "plan_review");

    // A summary, then its send: refused without confirmation, the question shows the exact text,
    // and sent once when confirmed.
    summary_answer(&bench, "La HU espera la revisión del plan.");
    let written = tool("workflow_summary", json!({"run": id})).await.unwrap();
    let summary = written["summary"]["id"].as_str().unwrap().to_owned();
    let send = json!({"run": id, "summary": summary, "chat": TEAM, "mentions": ["Ana Pérez"]});
    assert!(tool("workflow_summary_send", send.clone()).await.is_err());
    assert!(teams.posts().await.is_empty());
    let question =
        crate::app::tools::question_with(dispatch.clone(), "workflow_summary_send", &send)
            .await
            .unwrap()
            .unwrap();
    assert!(question.contains("La HU espera la revisión del plan.") && question.contains(TEAM));
    let mut confirmed = send.clone();
    confirmed["confirmed"] = json!(true);
    tool("workflow_summary_send", confirmed).await.unwrap();
    assert_eq!(teams.posts().await.len(), 1);

    // Cancel: refused without confirmation, then the run is cancelled.
    assert!(
        tool("workflow_run_cancel", json!({"run": id}))
            .await
            .is_err()
    );
    assert_eq!(bench.run(&id).state, Phase::PlanReview);
    tool("workflow_run_cancel", json!({"run": id, "confirmed": true}))
        .await
        .unwrap();
    assert_eq!(bench.run(&id).state, Phase::Cancelled);

    // The CLI's own words for the same operations are accepted by its argument check.
    use crate::app::cli::validate_args;
    let words = |args: &[&str]| -> Vec<String> { args.iter().map(|a| a.to_string()).collect() };
    for args in [
        words(&["workflows", "runs"]),
        words(&["workflows", "status", &id]),
        words(&["workflows", "resume", &id, "--confirmed"]),
        words(&["workflows", "cancel", &id, "--confirmed"]),
        words(&["workflows", "assign", &id, "payments", "--confirmed"]),
        words(&["workflows", "summary", &id]),
        words(&[
            "workflows",
            "summary",
            &id,
            "--send",
            TEAM,
            "--mention",
            "Ana Pérez",
            "--summary",
            &summary,
            "--confirmed",
        ]),
    ] {
        validate_args(&args).unwrap_or_else(|e| panic!("{args:?}: {e:?}"));
    }
    assert!(bench.catalog.only_reads());
}

/// One call of a lead's tool, as the fake provider CLI sends it in its turn.
fn lead_call(n: usize, name: &str, arguments: Value) -> String {
    json!({"type":"control_request","request_id":format!("m{n}"),"request":{"subtype":"mcp_message","server_name":"kuroko",
        "message":{"jsonrpc":"2.0","id":n,"method":"tools/call","params":{"name":name,"arguments":arguments}}}})
    .to_string()
}

/// The lead's shift now.
fn lead_shift(bench: &Bench) -> super::shifts::Shift {
    bench
        .host
        .state
        .workflows
        .shifts
        .list(Some("pagos"), 1)
        .unwrap()
        .remove(0)
}

/// Give the lead `calls` for its next turn, ask it to look, and step until `done` holds.
async fn lead_turn(bench: &Bench, calls: &[String], done: impl Fn(&Bench) -> bool) {
    fs::write(bench.log.join("lead_call"), calls.join("\n") + "\n").unwrap();
    for _ in 0..400 {
        let _ =
            super::shifts::operation(&bench.host, "workflow_lead_wake", &json!({"lead": "pagos"}));
        bench.tick().await;
        if done(bench) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!(
        "the lead's turn did not get there: {:?}",
        lead_shift(bench).wakes
    );
}

/// A workflow lead's whole shift out of shadow, with synthetic providers only: a task is coded
/// and its pull request opened; the lead moves the item to review, creates the pull request's
/// review support task (a second try is refused), closes the run by the workflow's completion
/// criteria, and ends its shift with its record. Every write to a provider is counted: one
/// change of the item's state and one work item, each once, nothing else.
#[tokio::test]
async fn a_leads_whole_shift_writes_only_what_its_workflow_allows_and_each_once() {
    use crate::workflows::lead::{Lead, May, WorkflowLead};
    let bench = Bench::new().await;
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflow_leads = vec![Lead {
        id: "pagos".into(),
        name: "Pagos".into(),
        agent: crate::workflows::Agent {
            provider: "claude".into(),
            model: "model".into(),
            effort: "high".into(),
            time_limit_minutes: None,
        },
        enabled: true,
        ..Default::default()
    }];
    map.workflows[0].lead = Some(WorkflowLead {
        id: "pagos".into(),
        may: vec![May::ItemStates, May::ProcessItems, May::Close],
        shadow: false,
    });
    map.workflows[0].done_when =
        "Done once its pull request is open for review with its support task.".into();
    bench.host.state.settings.save_map(&map).unwrap();
    crate::store::organization::OrganizationStore::save_guide(
        &crate::store::organization::LocalOrganization::new(&bench.host.state.settings),
        &crate::work_items::Guide {
            id: "support".into(),
            provider: "azure".into(),
            kind: "Task".into(),
            name: "Revisión PR".into(),
            ..Default::default()
        },
    )
    .unwrap();
    bench.mode("mode", "done");
    let id = bench.start(TASK).await;
    let url = "https://dev.azure.com/example/Payments/_git/payments-web/pullrequest/21";
    bench.catalog.set_pulls(
        WEB,
        "feature/child-1",
        vec![crate::ado::workflow::PullRequest {
            id: 21,
            repository: WEB.into(),
            title: "PR 21".into(),
            status: "active".into(),
            url: url.into(),
            created_at: Some((chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339()),
            auto_complete: Some(true),
            delete_source_branch: Some(true),
            ..Default::default()
        }],
    );
    bench.until(&id, |r| !r.support_due.is_empty()).await;

    // One turn: the item to review, its support task, and the same support task again.
    let support = json!({"run": id, "pull_request": url, "type": "Task", "title": "Revisión PR 21",
        "description": "Review pull request 21.", "reason": "Not approved by itself."});
    lead_turn(
        &bench,
        &[
            lead_call(
                1,
                "lead_set_item_state",
                json!({"run": id, "state": "En revisión", "reason": "Its pull request is open."}),
            ),
            lead_call(2, "lead_create_process_item", support.clone()),
            lead_call(3, "lead_create_process_item", support),
        ],
        |b| {
            let run = b.run(&id);
            !run.lead_writes.is_empty() && !run.process_items.is_empty()
        },
    )
    .await;
    let run = bench.run(&id);
    assert_eq!(run.lead_writes[0]["outcome"], "written");
    assert_eq!(run.process_items.len(), 1);
    assert_eq!(run.process_items[0]["state"], "created");

    // The next: the run is done by its criteria, its pull request still open.
    lead_turn(
        &bench,
        &[lead_call(
            4,
            "lead_close_run",
            json!({"run": id, "reason": "Open for review with its support task."}),
        )],
        |b| b.run(&id).state == Phase::Done,
    )
    .await;

    // The closing turn writes its record, and the shift ends.
    bench.offset.store(600, std::sync::atomic::Ordering::SeqCst);
    lead_turn(
        &bench,
        &[lead_call(
            5,
            "lead_finish",
            json!({"summary": "Coded, moved to review, its support task created and closed."}),
        )],
        |b| lead_shift(b).state == "ended",
    )
    .await;
    let ended = lead_shift(&bench);
    assert_eq!(
        ended.record.as_ref().unwrap()["summary"],
        "Coded, moved to review, its support task created and closed."
    );
    let decisions: Vec<(String, String)> = ended
        .journal
        .iter()
        .filter(|e| e.kind == "decision")
        .map(|e| {
            (
                e.decision.clone().unwrap_or_default(),
                e.result.clone().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        decisions[0],
        ("set_item_state".into(), "applied".into()),
        "{decisions:?}"
    );
    assert_eq!(
        decisions[1],
        ("create_process_item".into(), "applied".into()),
        "{decisions:?}"
    );
    assert!(
        decisions[2].1.contains("not due") || decisions[2].1.contains("never sent twice"),
        "{decisions:?}"
    );
    assert_eq!(
        decisions[3],
        ("close".into(), "applied".into()),
        "{decisions:?}"
    );

    // Every write to a provider: the item's state once, one work item once.
    let writes: Vec<String> = bench
        .catalog
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| !r.starts_with("GET "))
        .cloned()
        .collect();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(
        writes[0].starts_with(&format!("PATCH workitems/{TASK} ")),
        "{writes:?}"
    );
    assert!(writes[1].starts_with("POST workitems "), "{writes:?}");
    bench.release();
}
