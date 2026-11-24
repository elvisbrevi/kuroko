//! The retired workflow supervisor: its operations answer that the lead replaced it, what it
//! looked at and audited is still read, the line for slots is still the person's to reorder,
//! and a map that still names it or its pre-approvals loads and saves without them.
use super::*;
use crate::app::workflows::runs::tests::{Bench, TASK};
use crate::workflows::Repository;
use record::Pause;

const API: &str = "0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f";
const MACHINE: &str = "5a6b7c8d-9e0f-4a1b-8c2d-3e4f5a6b7c8d";

fn item(id: u64) -> record::Item {
    record::Item {
        provider: "azure".into(),
        id,
        project: "Payments".into(),
        organization: "https://dev.azure.com/example".into(),
        url: format!("https://dev.azure.com/example/Payments/_workitems/edit/{id}"),
        title: format!("Item {id}"),
        kind: "Task".into(),
        category: "task".into(),
        state: "Ready".into(),
        source: None,
    }
}
fn waiting(id: u64, at: i64) -> Run {
    let mut run = Run::new(item(id), at);
    run.workflow = Some("payments".into());
    run.definition = Some(crate::workflows::Workflow {
        id: "payments".into(),
        machine: MACHINE.into(),
        repositories: vec![Repository {
            id: API.into(),
            name: "payments-api".into(),
        }],
        ..Default::default()
    });
    run.advance(Phase::Coding, at, "assigned");
    run
}

#[test]
fn a_run_moved_ahead_goes_first_and_the_others_keep_their_order() {
    let now = 1_800_000_000;
    let mut runs: Vec<Run> = (1..=4).map(|n| waiting(n, now + n as i64)).collect();
    let order = |runs: &[Run]| {
        record::line(runs, now + 100, &[])
            .iter()
            .map(|r| r.item.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&runs), [1, 2, 3, 4]);
    runs[3].ahead_since = Some(now + 50);
    runs[2].ahead_since = Some(now + 60);
    assert_eq!(order(&runs), [4, 3, 1, 2]);
    // A paused run is out of the line, moved ahead or not.
    let pause = Pause {
        project: Some("Payments".into()),
        sessions: record::Sessions::Finish,
        paused_at: now,
    };
    assert!(record::line(&runs, now + 100, &[pause]).is_empty());
}

#[tokio::test]
async fn the_retired_supervisor_answers_that_the_lead_replaced_it_and_its_history_is_read() {
    let bench = Bench::new().await;
    bench.mode("mode", "fail");
    let id = bench.start(TASK).await;
    bench.state(&id, Phase::NeedsAttention).await;
    for (method, args) in [
        ("workflow_supervise", json!({"run": id})),
        (
            "workflow_supervision_act",
            json!({"run": id, "look": "l1", "action": "a1", "confirmed": true}),
        ),
        (
            "workflow_supervision_dismiss",
            json!({"run": id, "look": "l1", "action": "a1"}),
        ),
    ] {
        let refused = operation(&bench.host, method, &args).await.unwrap_err();
        assert!(refused.contains(RETIRED), "{method}: {refused}");
    }
    // A look an earlier version stored on the run, and its audit, are still read.
    let mut run = bench.run(&id);
    run.supervision.push(Look {
        id: "l1".into(),
        event: "run_stopped".into(),
        created_at: 1,
        outcome: "done".into(),
        summary: "Coded; its pull request is left.".into(),
        ..Default::default()
    });
    bench.host.state.workflows.db.save(&run).unwrap();
    bench
        .host
        .state
        .workflows
        .supervision
        .looks
        .insert_audit(&audit::Audit {
            id: "a1".into(),
            event: "batch_ended".into(),
            created_at: 2,
            outcome: "done".into(),
            ..Default::default()
        })
        .unwrap();
    let looks = operation(&bench.host, "workflow_supervision", &json!({"run": id}))
        .await
        .unwrap();
    assert_eq!(
        looks["looks"][0]["summary"],
        "Coded; its pull request is left."
    );
    let audits = operation(
        &bench.host,
        "workflow_supervision",
        &json!({"audits": true}),
    )
    .await
    .unwrap();
    assert_eq!(audits["audits"][0]["id"], "a1");
    let latest = operation(
        &bench.host,
        "workflow_supervision",
        &json!({"audit": "latest"}),
    )
    .await
    .unwrap();
    assert_eq!(latest["audit"]["id"], "a1");
    bench.release();
}

#[tokio::test]
async fn a_waiting_run_is_moved_ahead_and_back_by_the_person() {
    let bench = Bench::new().await;
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflow_settings.concurrency = 1;
    bench.host.state.settings.save_map(&map).unwrap();
    bench.mode("mode", "block");
    let first = bench.start(TASK).await;
    bench.until(&first, |r| r.admitted).await;
    bench.catalog.add(
        super::super::synthetic::detail(201, "Task", "task", "Second", "In payments-web."),
        true,
    );
    let second = bench.start(201).await;
    bench.tick().await;
    assert!(bench.run(&second).waits_for_slot());
    let requeue = |run: &str, ahead: bool| {
        let args = json!({"run": run, "ahead": ahead});
        let host = bench.host.clone();
        async move { operation(&host, "workflow_run_requeue", &args).await }
    };
    let refused = requeue(&first, true).await.unwrap_err();
    assert!(refused.contains("does not wait for a slot"), "{refused}");
    assert_eq!(requeue(&second, true).await.unwrap()["ahead"], true);
    assert_eq!(requeue(&second, false).await.unwrap()["ahead"], false);
    bench.release();
}
