//! A run waits for the dependencies of its item, end to end with the engine's synthetic catalog
//! and fake provider CLI: without a slot, judged again every watch interval, going on by itself
//! once the predecessor's pull request is completed; a cycle or a predecessor that cannot be
//! read needs a person; the person may skip the wait; a restart of the host changes nothing.
use super::*;
use crate::ado::dependencies::Node;

const SECOND: u64 = 201;

fn node(id: u64, state: &str, category: &str) -> Node {
    Node {
        id,
        url: format!("https://dev.azure.com/example/Payments/_workitems/edit/{id}"),
        title: format!("Task {id}"),
        kind: "Task".into(),
        project: "Payments".into(),
        state: state.into(),
        category: category.into(),
        ..Default::default()
    }
}

/// The bench with a second task that needs the first (a Predecessor link), the first in
/// progress, the watch interval one minute.
async fn bench_with_dependency() -> Bench {
    let bench = Bench::new().await;
    bench.catalog.add(
        synthetic::detail(SECOND, "Task", "task", "Second task", "In payments-web."),
        true,
    );
    bench.catalog.link(SECOND, TASK);
    bench
        .catalog
        .set_node(node(TASK, "In Progress", "InProgress"));
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflow_settings.watch_interval_minutes = 1;
    bench.host.state.settings.save_map(&map).unwrap();
    bench
}

/// Let one watch interval pass.
fn later(bench: &Bench) {
    bench.offset.fetch_add(61, Ordering::SeqCst);
}

#[tokio::test]
async fn a_run_waits_without_a_slot_for_its_predecessors_pull_request_and_then_goes_on() {
    let bench = bench_with_dependency().await;
    bench.mode("mode", "block");
    let first = bench.start(TASK).await;
    let second = bench.start(SECOND).await;
    // The predecessor has no dependency of its own and is coded; the successor first waits
    // for the checkouts its agent holds.
    bench
        .until(&first, |r| {
            r.children[0].coding.as_ref().is_some_and(|c| c.started)
        })
        .await;
    bench
        .until(&second, |r| {
            r.error
                .as_deref()
                .is_some_and(|e| e.contains("Waiting for the checkouts"))
        })
        .await;
    // The predecessor is coded and opens its pull request: the successor now waits for it,
    // without a slot and without a coding run, naming what it waits for.
    bench.mode("mode", "done");
    bench.release();
    bench.pulls(WEB, "payments-web", "feature/child-1", 41, "active");
    bench.state(&first, Phase::PullRequests).await;
    let waiting = bench.state(&second, Phase::WaitingDependency).await;
    assert!(!waiting.admitted);
    assert!(waiting.children[0].coding.is_none());
    let wait = waiting.wait.clone().unwrap();
    assert_eq!((wait.subject, wait.continue_to), (SECOND, Phase::Coding));
    assert!(
        wait.text.contains("#200 (Pull request !41 is active)"),
        "{}",
        wait.text
    );
    let dependency = &waiting.children[0].dependencies[0];
    assert_eq!((dependency.id, dependency.satisfied), (TASK, false));
    assert_eq!(
        dependency.url,
        "https://dev.azure.com/example/Payments/_workitems/edit/200"
    );
    let listed = bench.op("workflow_runs", json!({})).await.unwrap();
    let shown = listed["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == second.as_str())
        .unwrap();
    assert_eq!(shown["state"], "waiting_dependency");
    assert_eq!(shown["waiting_for"]["subject"], SECOND);
    assert_eq!(shown["waits_on_user"], false);
    // Judged again after an interval: still active, still waiting.
    later(&bench);
    bench.tick().await;
    let waiting = bench.run(&second);
    assert_eq!(waiting.state, Phase::WaitingDependency);
    assert!(waiting.wait.as_ref().unwrap().checked_at > wait.checked_at);
    // Within the interval nothing is read again.
    let reads = bench.reads("dependencies 201");
    bench.tick().await;
    assert_eq!(bench.reads("dependencies 201"), reads);
    // Its pull request is completed: the successor goes on by itself and is coded.
    bench.pulls(WEB, "payments-web", "feature/child-1", 41, "completed");
    bench.state(&first, Phase::Done).await;
    later(&bench);
    let run = bench.state(&second, Phase::PullRequests).await;
    assert!(run.children[0].dependencies[0].satisfied);
    assert_eq!(run.children[0].dependencies[0].rule, "pull_requests");
    let stages: Vec<&str> = run.timeline.iter().map(|s| s.stage.word()).collect();
    assert_eq!(
        stages,
        [
            "classifying",
            "coding",
            "waiting_dependency",
            "coding",
            "pull_requests"
        ]
    );
    assert_eq!(bench.count("starts"), 2);
}

#[tokio::test]
async fn a_cycle_needs_attention_with_the_cycle_described() {
    let bench = bench_with_dependency().await;
    // 201 needs 200, and 200 needs 201.
    bench.catalog.link(TASK, SECOND);
    bench.catalog.set_node(node(SECOND, "Ready", "Proposed"));
    let id = bench.start(SECOND).await;
    let run = bench.state(&id, Phase::NeedsAttention).await;
    let attention = run.attention.unwrap();
    assert!(attention.contains("#201 → #200 → #201"), "{attention}");
    assert_eq!(run.cause.as_deref(), Some("dependency"));
    assert!(run.children[0].coding.is_none());
    // Resumed without a change, it stops again: it never waits for ever.
    bench
        .op("workflow_run_resume", json!({"run": id, "confirmed": true}))
        .await
        .unwrap();
    let run = bench.state(&id, Phase::NeedsAttention).await;
    assert!(run.attention.unwrap().contains("cycle"));
    assert_eq!(bench.count("starts"), 0);
}

#[tokio::test]
async fn a_missing_predecessor_needs_attention_and_the_person_may_skip_it() {
    let bench = bench_with_dependency().await;
    // 201 also needs 999, which does not exist.
    bench.catalog.link(SECOND, 999);
    bench.catalog.set_node(node(TASK, "Done", "Completed"));
    let id = bench.start(SECOND).await;
    let run = bench.state(&id, Phase::NeedsAttention).await;
    assert!(run.attention.unwrap().contains("#999 does not exist"));
    // Skipping needs confirmation and names what it lets go.
    let refused = bench
        .op(
            "workflow_run_skip_dependency",
            json!({"run": id, "dependency": 999}),
        )
        .await
        .unwrap_err();
    assert!(refused.contains("dependency #999 is"), "{refused}");
    bench
        .op(
            "workflow_run_skip_dependency",
            json!({"run": id, "dependency": 999, "confirmed": true}),
        )
        .await
        .unwrap();
    let run = bench.state(&id, Phase::PullRequests).await;
    let skipped = run.children[0]
        .dependencies
        .iter()
        .find(|d| d.id == 999)
        .unwrap();
    assert_eq!(skipped.rule, "skipped");
    assert_eq!(bench.count("starts"), 1);
}

#[tokio::test]
async fn skipping_the_wait_goes_on_at_once_and_a_run_that_does_not_wait_is_refused() {
    let bench = bench_with_dependency().await;
    let id = bench.start(SECOND).await;
    bench.state(&id, Phase::WaitingDependency).await;
    let skipped = bench
        .op(
            "workflow_run_skip_dependency",
            json!({"run": id, "confirmed": true}),
        )
        .await
        .unwrap();
    assert_eq!(skipped["run"]["state"], "coding");
    let run = bench.state(&id, Phase::PullRequests).await;
    assert_eq!(run.dependencies_met, Some(SECOND));
    let refused = bench
        .op(
            "workflow_run_skip_dependency",
            json!({"run": id, "confirmed": true}),
        )
        .await
        .unwrap_err();
    assert!(
        refused.contains("does not wait for its dependencies"),
        "{refused}"
    );
}

#[tokio::test]
async fn a_wait_survives_a_restart_of_the_host_and_a_long_one_needs_a_person() {
    let mut bench = bench_with_dependency().await;
    let id = bench.start(SECOND).await;
    let waited = bench.state(&id, Phase::WaitingDependency).await;
    bench.restart().await;
    later(&bench);
    bench.tick().await;
    let run = bench.run(&id);
    assert_eq!(run.state, Phase::WaitingDependency);
    assert_eq!(run.wait.as_ref().unwrap().since, waited.wait.unwrap().since);
    assert!(run.children[0].coding.is_none());
    // Past dependency_wait_days a person must look.
    bench.offset.fetch_add(14 * 86_400, Ordering::SeqCst);
    let run = bench.state(&id, Phase::NeedsAttention).await;
    assert!(run.attention.as_deref().unwrap().contains("within 14 days"));
    assert_eq!(run.stopped_in, Some(Phase::WaitingDependency));
    // Resumed, it waits again from now (the operation's clock); met, it goes on.
    bench.offset.store(0, Ordering::SeqCst);
    bench
        .op("workflow_run_resume", json!({"run": id, "confirmed": true}))
        .await
        .unwrap();
    bench.state(&id, Phase::WaitingDependency).await;
    bench.catalog.set_node(node(TASK, "Done", "Completed"));
    later(&bench);
    bench.state(&id, Phase::PullRequests).await;
    assert_eq!(bench.count("starts"), 1);
}

#[tokio::test]
async fn an_item_without_links_reads_its_dependencies_once_and_is_coded_as_before() {
    let bench = Bench::new().await;
    let id = bench.start(TASK).await;
    let run = bench.state(&id, Phase::PullRequests).await;
    assert!(run.wait.is_none() && run.children[0].dependencies.is_empty());
    assert_eq!(bench.reads("dependencies 200"), 1);
    let stages: Vec<&str> = run.timeline.iter().map(|s| s.stage.word()).collect();
    assert_eq!(stages, ["classifying", "coding", "pull_requests"]);
}
