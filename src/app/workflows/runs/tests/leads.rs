//! A workflow lead's shift end to end, with a fake Claude: opened when a run of its workflow is
//! active, woken by what changed with the opening and the facts, its decisions recorded beside
//! the run's state and never applied, a person's request answered at once, and ended with its
//! record when the last run has ended.
use super::*;
use crate::workflows::lead::{Lead, WorkflowLead};

fn lead_to(bench: &Bench) {
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflow_leads = vec![Lead {
        id: "pagos".into(),
        name: "Pagos".into(),
        description: "Say what is left in plain words.".into(),
        agent: Agent {
            provider: "claude".into(),
            model: "model".into(),
            effort: "high".into(),
            time_limit_minutes: None,
        },
        enabled: true,
        updated_at: 0,
    }];
    map.workflows[0].lead = Some(WorkflowLead { id: "pagos".into() });
    map.workflows[0].done_when = "Done once its pull request is completed.".into();
    bench.host.state.settings.save_map(&map).unwrap();
}

fn shift(bench: &Bench) -> super::super::shifts::Shift {
    bench
        .host
        .state
        .workflows
        .shifts
        .list(Some("pagos"), 1)
        .unwrap()
        .remove(0)
}

/// Tick until the lead's shift has `turns` turns.
async fn turns(bench: &Bench, turns: usize) -> super::super::shifts::Shift {
    for _ in 0..400 {
        bench.tick().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let found = bench
            .host
            .state
            .workflows
            .shifts
            .list(Some("pagos"), 1)
            .unwrap();
        if found.first().is_some_and(|s| s.wakes.len() >= turns) {
            return found.into_iter().next().unwrap();
        }
    }
    panic!("the lead took no turn {turns}");
}

#[tokio::test]
async fn a_lead_follows_its_workflows_runs_in_shadow_and_ends_its_shift_with_a_record() {
    let bench = Bench::new().await;
    lead_to(&bench);
    // The organization's norm for its project reaches its opening.
    crate::store::organization::OrganizationStore::save_entry(
        &crate::store::organization::LocalOrganization::new(&bench.host.state.settings),
        &crate::organization::Entry {
            id: "pr-norm".into(),
            kind: crate::organization::Kind::Norm,
            title: "Pull requests".into(),
            text: "Every pull request completes itself and deletes its source branch.".into(),
            projects: Vec::new(),
            updated_at: 0,
        },
    )
    .unwrap();
    bench.mode("mode", "block");
    let id = bench.start(TASK).await;
    bench
        .until(&id, |r| {
            r.state == Phase::Coding && r.children[0].coding.as_ref().is_some_and(|c| c.started)
        })
        .await;
    // Its turn decides, reads a Wiki page and a file of its workflow, and asks the person.
    bench
        .catalog
        .set_text("page:/Procedures/Deploy", "Deploy steps");
    bench
        .catalog
        .set_text(&format!("{WEB}:/src/Rounding.cs"), "class Rounding {}");
    let calls = [
        json!({"name":"lead_decide","arguments":{"run":id,"decision":"wait","reason":"Its agent is still coding."}}),
        json!({"name":"lead_wiki_page","arguments":{"workflow":"payments","path":"/Procedures/Deploy"}}),
        json!({"name":"lead_repository_file","arguments":{"workflow":"payments","repository":"payments-web","path":"/src/Rounding.cs"}}),
        json!({"name":"lead_repository_file","arguments":{"workflow":"other","repository":"payments-web","path":"/x"}}),
        json!({"name":"lead_ask_person","arguments":{"run":id,"question":"Is the legacy rounding kept?"}}),
    ];
    let lines: Vec<String> = calls
        .iter()
        .enumerate()
        .map(|(n, params)| json!({"type":"control_request","request_id":format!("m{n}"),"request":{"subtype":"mcp_message","server_name":"kuroko",
            "message":{"jsonrpc":"2.0","id":n,"method":"tools/call","params":params}}}).to_string())
        .collect();
    fs::write(bench.log.join("lead_call"), lines.join("\n") + "\n").unwrap();
    let first = turns(&bench, 1).await;
    let replies = fs::read_to_string(bench.log.join("lead_replies")).unwrap();
    let replies: Vec<&str> = replies.lines().collect();
    assert!(replies[1].contains("Deploy steps"), "{}", replies[1]);
    assert!(replies[2].contains("class Rounding"), "{}", replies[2]);
    assert!(
        replies[3].contains("not one of your workflows"),
        "{}",
        replies[3]
    );
    assert!(replies[4].contains("asked"), "{}", replies[4]);
    assert_eq!(first.state, "active");
    assert_eq!(first.wakes[0].outcome, "done", "{:?}", first.wakes);
    assert_eq!(first.session_id.as_deref(), Some("ses_lead"));
    // The opening: its rules, who it is, its workflow's criteria, what happened and the facts.
    let prompt = fs::read_to_string(bench.log.join("lead_prompts")).unwrap();
    for expected in [
        "You are Pagos",
        "Say what is left",
        "Done once its pull request is completed",
        "is now yours to follow",
        "Your runs now",
        "completes itself and deletes its source branch",
    ] {
        assert!(prompt.contains(expected), "{expected}: {prompt}");
    }
    // Its decision is recorded beside the run's state, never applied.
    let decision = first.journal.iter().find(|e| e.kind == "decision").unwrap();
    assert_eq!(decision.decision.as_deref(), Some("wait"));
    assert_eq!(decision.engine.as_deref(), Some("coding"));
    assert_eq!(bench.run(&id).state, Phase::Coding);
    // Listed brief: its state, last look and how many decisions, without its journal.
    let listed =
        super::super::shifts::operation(&bench.host, "workflow_lead_shifts", &json!({})).unwrap();
    assert_eq!(listed["shifts"][0]["decisions"], 1, "{listed}");
    assert!(listed["shifts"][0].get("journal").is_none());
    // Its question waits for the person, on Home too.
    let question = listed["shifts"][0]["questions"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let attention = super::super::shifts::attention(&bench.host.state).unwrap();
    assert_eq!(attention[0]["kind"], "question");
    assert_eq!(attention[0]["text"], "Is the legacy rounding kept?");
    fs::remove_file(bench.log.join("lead_call")).unwrap();
    // Nothing new and within the interval: no other turn.
    bench.tick().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(shift(&bench).wakes.len(), 1);
    // A person's request is answered at once, in the same conversation.
    super::super::shifts::operation(&bench.host, "workflow_lead_wake", &json!({"lead": "Pagos"}))
        .unwrap();
    let second = turns(&bench, 2).await;
    assert_eq!(second.wakes[1].outcome, "done");
    let args = fs::read_to_string(bench.log.join("lead_args.1")).unwrap();
    assert!(args.contains("--resume\nses_lead"), "{args}");
    // The person's answer reaches it at once, at its next turn.
    super::super::shifts::operation(
        &bench.host,
        "workflow_lead_answer",
        &json!({"lead": "pagos", "question": question, "answer": "Yes, keep it."}),
    )
    .unwrap();
    // A second answer on its way is refused, not dropped.
    let twice = super::super::shifts::operation(
        &bench.host,
        "workflow_lead_answer",
        &json!({"lead": "pagos", "question": question, "answer": "No."}),
    )
    .unwrap_err();
    assert!(twice.contains("has an answer already"), "{twice}");
    let third = turns(&bench, 3).await;
    let prompts = fs::read_to_string(bench.log.join("lead_prompts")).unwrap();
    assert!(
        prompts
            .lines()
            .last()
            .unwrap()
            .contains("«Is the legacy rounding kept?»: Yes, keep it."),
        "{prompts}"
    );
    let asked = third.journal.iter().find(|e| e.kind == "question").unwrap();
    assert_eq!(asked.answer.as_deref(), Some("Yes, keep it."));
    assert!(
        super::super::shifts::attention(&bench.host.state)
            .unwrap()
            .is_empty()
    );
    // An answered question takes no other answer.
    super::super::shifts::operation(
        &bench.host,
        "workflow_lead_answer",
        &json!({"lead": "pagos", "question": question, "answer": "Again."}),
    )
    .unwrap_err();
    // The run ends: the closing turn ends the shift with its record.
    bench.release();
    bench.state(&id, Phase::PullRequests).await;
    bench.pulls(WEB, "payments-web", "feature/child-1", 21, "completed");
    bench.state(&id, Phase::Done).await;
    bench.offset.store(600, Ordering::SeqCst);
    let mut ended = shift(&bench);
    for _ in 0..400 {
        bench.tick().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        ended = shift(&bench);
        if ended.state == "ended" {
            break;
        }
    }
    assert_eq!(ended.state, "ended", "{:?}", ended.wakes);
    let record = ended.record.unwrap();
    assert_eq!(record["summary"], "The lead wrote no summary.");
    assert_eq!(record["runs"][0]["state"], "done");
    // Its runs were driven by the engine alone, and nothing was written.
    assert_eq!(bench.count("starts"), 1);
    assert!(bench.catalog.only_reads());
}

#[tokio::test]
async fn a_lead_turned_off_ends_its_shift_without_a_turn_and_takes_no_request() {
    let bench = Bench::new().await;
    lead_to(&bench);
    bench.mode("mode", "block");
    let id = bench.start(TASK).await;
    bench
        .until(&id, |r| {
            r.children[0].coding.as_ref().is_some_and(|c| c.started)
        })
        .await;
    turns(&bench, 1).await;
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflow_leads[0].enabled = false;
    bench.host.state.settings.save_map(&map).unwrap();
    bench.tick().await;
    let ended = shift(&bench);
    assert_eq!(ended.state, "ended");
    assert_eq!(ended.wakes.len(), 1);
    assert_eq!(ended.record.unwrap()["runs"][0]["run"], id.as_str());
    let refused = super::super::shifts::operation(
        &bench.host,
        "workflow_lead_wake",
        &json!({"lead": "pagos"}),
    )
    .unwrap_err();
    assert!(refused.contains("is off"), "{refused}");
    bench.release();
}

#[tokio::test]
async fn a_turn_the_host_did_not_finish_is_marked_and_taken_again() {
    let bench = Bench::new().await;
    lead_to(&bench);
    bench.mode("mode", "block");
    let id = bench.start(TASK).await;
    bench
        .until(&id, |r| {
            r.children[0].coding.as_ref().is_some_and(|c| c.started)
        })
        .await;
    let mut cut = turns(&bench, 1).await;
    // A host that stopped in the middle of a turn leaves its mark.
    cut.turn_started = Some(cut.wakes[0].at);
    bench.host.state.workflows.shifts.save(&cut).unwrap();
    // Taken again at its next turn, past the interval.
    bench.offset.store(600, Ordering::SeqCst);
    let again = turns(&bench, 3).await;
    assert_eq!(
        again.wakes[1].outcome, "host_restarted",
        "{:?}",
        again.wakes
    );
    assert_eq!(again.wakes[2].outcome, "done");
    assert!(again.turn_started.is_none());
    let prompts = fs::read_to_string(bench.log.join("lead_prompts")).unwrap();
    assert!(
        prompts
            .lines()
            .last()
            .unwrap()
            .contains("did not finish (Kuroko restarted)"),
        "{prompts}"
    );
    bench.release();
}
