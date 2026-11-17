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
    assert!(
        run.attention
            .as_deref()
            .unwrap()
            .contains("more than 14 days")
    );
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

#[tokio::test]
async fn a_planned_child_that_depends_on_an_earlier_one_waits_for_its_pull_request() {
    let bench = Bench::new().await;
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflow_settings.watch_interval_minutes = 1;
    bench.host.state.settings.save_map(&map).unwrap();
    // The plan's third child depends on the first: the creation linked it (Predecessor).
    bench.catalog.add(
        synthetic::detail(103, "Task", "task", "Receipt totals", "In payments-web."),
        true,
    );
    bench.catalog.link(103, 101);
    bench
        .catalog
        .set_node(node(101, "In Progress", "InProgress"));
    let id = bench.start(HU).await;
    let run = bench.state(&id, Phase::PlanReview).await;
    // The first child's pull request is open from the start; the second's is completed.
    bench.pulls(WEB, "payments-web", "feature/child-1", 11, "active");
    bench.pulls(API, "payments-api", "feature/child-2", 12, "completed");
    bench.confirm(&run, "created", &[101, 102, 103]).await;
    // The first two are coded in order; the third waits, without a slot, for the first's PR.
    let waiting = bench.state(&id, Phase::WaitingDependency).await;
    assert_eq!(bench.count("starts"), 2);
    assert!(!waiting.admitted);
    let wait = waiting.wait.clone().unwrap();
    assert_eq!((wait.subject, wait.continue_to), (103, Phase::Coding));
    assert!(
        wait.text.contains("#101 (Pull request !11 is active)"),
        "{}",
        wait.text
    );
    assert_eq!(waiting.children[2].dependencies[0].id, 101);
    assert!(waiting.children[2].coding.is_none());
    // Still active after an interval: still waiting, the sibling's pull request read again.
    let reads = bench.reads("feature/child-1");
    later(&bench);
    bench.tick().await;
    assert_eq!(bench.run(&id).state, Phase::WaitingDependency);
    assert!(bench.reads("feature/child-1") > reads);
    // Completed: the third child is coded, after both others.
    bench.pulls(WEB, "payments-web", "feature/child-1", 11, "completed");
    later(&bench);
    let run = bench.state(&id, Phase::PullRequests).await;
    assert_eq!(bench.count("starts"), 3);
    let coded: Vec<u64> = run.children.iter().map(|c| c.id).collect();
    assert_eq!(coded, vec![101, 102, 103]);
    assert!(run.children[2].started_at.unwrap() >= run.children[1].finished_at.unwrap());
    assert!(run.children[2].dependencies[0].satisfied);
    let stages: Vec<&str> = run.timeline.iter().map(|s| s.stage.word()).collect();
    assert!(
        stages.ends_with(&["coding", "waiting_dependency", "coding", "pull_requests"]),
        "{stages:?}"
    );
}

/// The bench's workflow with these delivery settings.
fn deliver(bench: &Bench, delivery: crate::workflows::Delivery) {
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflows[0].delivery = delivery;
    map.workflow_settings.watch_interval_minutes = 1;
    bench.host.state.settings.save_map(&map).unwrap();
}
/// One pull request of a branch, as the next read returns it.
fn pull(bench: &Bench, branch: &str, id: u64, status: &str, set: impl FnOnce(&mut PullRequest)) {
    let mut pull = PullRequest {
        id,
        repository: WEB.into(),
        title: format!("PR {id}"),
        status: status.into(),
        url: format!("https://dev.azure.com/example/Payments/_git/payments-web/pullrequest/{id}"),
        target: Some("refs/heads/main".into()),
        merge_status: Some("succeeded".into()),
        target_commit: Some("c0".into()),
        ..Default::default()
    };
    set(&mut pull);
    bench.catalog.set_pulls(WEB, branch, vec![pull]);
}

#[tokio::test]
async fn a_pull_request_left_in_conflict_by_another_runs_needs_attention_naming_the_files() {
    let bench = Bench::new().await;
    bench.catalog.add(
        synthetic::detail(SECOND, "Task", "task", "Second task", "In payments-web."),
        true,
    );
    let first = bench.start(TASK).await;
    let second = bench.start(SECOND).await;
    pull(&bench, "feature/child-1", 41, "active", |_| {});
    pull(&bench, "feature/child-2", 42, "active", |_| {});
    bench.state(&first, Phase::PullRequests).await;
    bench.state(&second, Phase::PullRequests).await;
    // The first is completed; the second now conflicts with main on two files.
    pull(&bench, "feature/child-1", 41, "completed", |p| {
        p.closed_at = Some("2026-11-16T12:00:00Z".into())
    });
    pull(&bench, "feature/child-2", 42, "active", |p| {
        p.merge_status = Some("conflicts".into());
        p.target_commit = Some("c1".into());
    });
    bench
        .catalog
        .set_conflicts(42, &["CHANGELOG.md", "package.json"]);
    let run = bench.state(&second, Phase::NeedsAttention).await;
    let attention = run.attention.clone().unwrap();
    assert!(
        attention.contains("pullrequest/42 conflicts with main in CHANGELOG.md, package.json"),
        "{attention}"
    );
    assert!(
        attention.contains(
            "pullrequest/41 of https://dev.azure.com/example/Payments/_workitems/edit/200"
        ),
        "{attention}"
    );
    assert_eq!(run.cause.as_deref(), Some("conflict"));
    assert_eq!(run.conflict.as_ref().unwrap().files.len(), 2);
    // Nothing was resolved and nothing written: only reads.
    assert_eq!(bench.state(&first, Phase::Done).await.state, Phase::Done);
    // Resumed, it waits on; the same conflict against the same commit does not stop it again.
    bench
        .op(
            "workflow_run_resume",
            json!({"run": second, "confirmed": true}),
        )
        .await
        .unwrap();
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(bench.run(&second).state, Phase::PullRequests);
    // The target branch moves and it still conflicts: it stops again.
    pull(&bench, "feature/child-2", 42, "active", |p| {
        p.merge_status = Some("conflicts".into());
        p.target_commit = Some("c2".into());
    });
    bench.state(&second, Phase::NeedsAttention).await;
}

#[tokio::test]
async fn a_workflow_limited_to_one_open_pull_request_codes_the_next_run_once_it_is_completed() {
    let bench = Bench::new().await;
    deliver(
        &bench,
        crate::workflows::Delivery {
            max_open_pull_requests: 1,
            ..Default::default()
        },
    );
    for id in [SECOND, 202] {
        bench.catalog.add(
            synthetic::detail(
                id,
                "Task",
                "task",
                &format!("Task {id}"),
                "In payments-web.",
            ),
            true,
        );
    }
    let first = bench.start(TASK).await;
    pull(&bench, "feature/child-1", 41, "active", |_| {});
    bench.state(&first, Phase::PullRequests).await;
    let second = bench.start(SECOND).await;
    let third = bench.start(202).await;
    // Without any declared dependency, the second waits for room; the third waits behind it.
    let waiting = bench.state(&second, Phase::WaitingDependency).await;
    let wait = waiting.wait.unwrap();
    assert_eq!(wait.reason, "open_pull_requests");
    assert!(wait.text.contains("pullrequest/41"), "{}", wait.text);
    assert!(!waiting.admitted && waiting.children[0].coding.is_none());
    let behind = bench.state(&third, Phase::WaitingDependency).await;
    assert_eq!(behind.wait.unwrap().reason, "open_pull_requests");
    assert_eq!(bench.count("starts"), 1);
    // The first pull request is completed: the second is coded, the third still waits for room.
    pull(&bench, "feature/child-1", 41, "completed", |_| {});
    bench.state(&first, Phase::Done).await;
    later(&bench);
    bench.state(&second, Phase::PullRequests).await;
    later(&bench);
    bench.tick().await;
    assert_eq!(bench.run(&third).state, Phase::WaitingDependency);
    assert_eq!(bench.count("starts"), 2);
}

#[tokio::test]
async fn a_stacked_child_starts_from_its_predecessors_branch_and_asks_to_be_retargeted() {
    let bench = bench_with_dependency().await;
    deliver(
        &bench,
        crate::workflows::Delivery {
            dependency_policy: crate::workflows::DependencyPolicy::Stack,
            ..Default::default()
        },
    );
    let first = bench.start(TASK).await;
    pull(&bench, "feature/child-1", 41, "active", |_| {});
    bench.state(&first, Phase::PullRequests).await;
    // The second needs the first, whose one pull request is open: it is stacked on its branch.
    let second = bench.start(SECOND).await;
    pull(&bench, "feature/child-2", 42, "active", |p| {
        p.target = Some("refs/heads/feature/child-1".into())
    });
    let run = bench.state(&second, Phase::PullRequests).await;
    let base = run.children[0].base.clone().unwrap();
    assert_eq!(
        (base.branch.as_str(), base.pull_request),
        ("feature/child-1", 41)
    );
    assert_eq!(run.children[0].dependencies[0].rule, "stacked");
    let prompt = fs::read_to_string(bench.log.join("coding.2")).unwrap();
    assert!(
        prompt.contains("Start your branch from feature/child-1"),
        "{prompt}"
    );
    // The first is completed: the stacked pull request still goes into its branch.
    pull(&bench, "feature/child-1", 41, "completed", |_| {});
    bench.state(&first, Phase::Done).await;
    let run = bench.state(&second, Phase::NeedsAttention).await;
    assert_eq!(run.cause.as_deref(), Some("retarget"));
    assert!(run.attention.unwrap().contains("retarget it"));
    // Retargeted by the person and resumed: it waits for its pull request again.
    bench
        .op(
            "workflow_run_resume",
            json!({"run": second, "confirmed": true}),
        )
        .await
        .unwrap();
    pull(&bench, "feature/child-2", 42, "completed", |_| {});
    bench.state(&second, Phase::Done).await;
}
