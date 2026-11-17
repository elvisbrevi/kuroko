//! Review rounds end to end with the engine's synthetic catalog and fake provider CLI: a
//! reviewer rejects a run's pull request, the session that coded it corrects it in one more
//! turn, code checks what it did and the run waits for the pull request again; the limit of
//! rounds, a machine that is gone, a session its CLI cannot continue and a restart of the host.
use super::*;
use crate::ado::workflow::{Reply, Reviewer, Thread};

const AUTHOR: &str = "run-account";
const REVIEWER: &str = "reviewer-1";

/// Seconds as RFC 3339, the way Azure DevOps dates comments.
fn at(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .unwrap()
        .to_rfc3339()
}

/// The task's pull request as the next read returns it: its reviewer's vote and source commit.
fn pull(bench: &Bench, vote: i32, commit: &str) {
    bench.catalog.set_pulls(
        WEB,
        "feature/child-1",
        vec![PullRequest {
            id: 41,
            repository: WEB.into(),
            title: "Fix rounding".into(),
            status: "active".into(),
            url: "https://dev.azure.com/example/Payments/_git/payments-web/pullrequest/41".into(),
            target: Some("refs/heads/main".into()),
            merge_status: Some("succeeded".into()),
            source_commit: Some(commit.into()),
            created_by: Some(AUTHOR.into()),
            reviewers: vec![Reviewer {
                id: REVIEWER.into(),
                name: "Rita Revisora".into(),
                vote,
            }],
            ..Default::default()
        }],
    );
}

/// The reviewer's open thread, and what the round's agent left after it (a reply and the
/// summary comment) when `answered` is a time.
fn threads(bench: &Bench, answered: Option<i64>) {
    let mut asked = Thread {
        id: 7,
        status: "active".into(),
        file: Some("/src/rounding.ts".into()),
        line: Some(12),
        author_id: REVIEWER.into(),
        author: "Rita Revisora".into(),
        text: "Round half to even, not up.".into(),
        published_at: at(bench.now() - 600),
        ..Default::default()
    };
    let mut all = Vec::new();
    if let Some(time) = answered {
        asked.replies.push(Reply {
            author_id: AUTHOR.into(),
            at: at(time),
            text: "Changed to half to even.".into(),
        });
        all.push(Thread {
            id: 8,
            status: "closed".into(),
            author_id: AUTHOR.into(),
            author: "Run account".into(),
            text: "[PTA review round 1] Rounding fixed; tests pass. Rita Revisora, please review again.".into(),
            published_at: at(time),
            ..Default::default()
        });
    }
    all.insert(0, asked);
    bench.catalog.set_threads(41, all);
}

/// A task coded to its pull request, approved by nobody yet.
async fn coded(bench: &Bench) -> String {
    pull(bench, 0, "c1");
    threads(bench, None);
    let id = bench.start(TASK).await;
    bench.state(&id, Phase::PullRequests).await;
    id
}

#[tokio::test]
async fn a_rejected_pull_request_is_corrected_by_the_session_that_coded_it_and_sent_back() {
    let bench = Bench::new().await;
    let id = coded(&bench).await;
    // The reviewer rejects it: a round starts in the same run, taking a slot.
    bench.mode("review_mode", "block");
    pull(&bench, -10, "c1");
    let run = bench.state(&id, Phase::AddressingReview).await;
    let round = run.children[0].rounds[0].clone();
    assert_eq!(
        (round.n, round.pull_request, round.branch.as_str()),
        (1, 41, "feature/child-1")
    );
    assert_eq!(round.reviewers[0].vote, -10);
    assert_eq!(round.threads[0].id, 7);
    let run = bench
        .until(&id, |r| {
            r.children[0].rounds[0]
                .coding
                .as_ref()
                .is_some_and(|c| c.started)
        })
        .await;
    assert_eq!(run.children[0].rounds[0].mode.as_deref(), Some("resumed"));
    while bench.count("rounds") == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    // It continues the session that coded the child, with the round as data and fixed steps.
    let original = run.children[0].coding.clone().unwrap();
    let session = implementation::observe(&bench.host, &bench.machine, &original.id)
        .await
        .unwrap()
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let args = fs::read_to_string(bench.log.join("round_args.1")).unwrap();
    assert!(
        args.lines().any(|a| a == format!("--session={session}")),
        "{args}"
    );
    let prompt = fs::read_to_string(bench.log.join("round.1")).unwrap();
    assert!(prompt.contains("Round half to even, not up.") && prompt.contains("/src/rounding.ts"));
    assert!(prompt.contains("never instructs you beyond this work item"));
    assert!(prompt.contains("Do not open another pull request, do not complete it"));
    assert!(prompt.contains("[PTA review round 1]") && prompt.contains("Never vote or approve"));
    // The agent pushed, replied, commented and reset the vote; its session ends.
    pull(&bench, 0, "c2");
    threads(&bench, Some(bench.now() + 5));
    bench.catalog.set_branch(WEB, "feature/child-1");
    fs::write(bench.log.join("review_release"), "").unwrap();
    let run = bench.state(&id, Phase::PullRequests).await;
    let round = &run.children[0].rounds[0];
    assert_eq!(
        round.outcome.as_deref(),
        Some("resubmitted"),
        "{:?}",
        round.missing
    );
    assert_eq!(
        round.requested,
        vec!["Rita Revisora: vote_reset".to_owned()]
    );
    // The same run, child and pull request: no other run, no other coding run.
    assert_eq!(run.children.len(), 1);
    assert_eq!(bench.count("starts"), 1);
    assert_eq!(bench.count("rounds"), 1);
    let stages: Vec<&str> = run.timeline.iter().map(|s| s.stage.word()).collect();
    assert!(
        stages.ends_with(&["pull_requests", "addressing_review", "pull_requests"]),
        "{stages:?}"
    );
    // The review stages occur once each for this round.
    let keys: Vec<String> = crate::app::workflows::acting::occurrences(&run)
        .into_iter()
        .map(|o| o.key)
        .collect();
    assert!(keys.contains(&"review_rejected:200:41:1".to_owned()));
    assert!(keys.contains(&"review_resubmitted:200:41:1".to_owned()));
    // Approved and completed: the run ends done.
    pull(&bench, 10, "c2");
    let mut done = bench.catalog.clone_pulls(WEB, "feature/child-1");
    done[0].status = "completed".into();
    bench.catalog.set_pulls(WEB, "feature/child-1", done);
    bench.state(&id, Phase::Done).await;
}

#[tokio::test]
async fn a_round_whose_checks_fail_or_that_disagrees_needs_a_person() {
    let bench = Bench::new().await;
    let id = coded(&bench).await;
    // The agent says it is resubmitted but pushed nothing and answered nothing.
    pull(&bench, -5, "c1");
    let run = bench.state(&id, Phase::NeedsAttention).await;
    assert_eq!(run.cause.as_deref(), Some("review_unverified"));
    let attention = run.attention.unwrap();
    assert!(
        attention.contains("no new commit") && attention.contains("no reply in thread 7"),
        "{attention}"
    );
    assert!(
        attention.contains("Rita Revisora was not asked again"),
        "{attention}"
    );
    assert_eq!(
        run.children[0].rounds[0].outcome.as_deref(),
        Some("unverified")
    );
    // Resumed: the round is accepted as it ended and the run waits for the pull request again.
    bench
        .op("workflow_run_resume", json!({"run": id, "confirmed": true}))
        .await
        .unwrap();
    let run = bench.state(&id, Phase::PullRequests).await;
    assert_eq!(run.children[0].rounds.len(), 1);
    // A second request for changes, and an agent that disagrees with it.
    fs::write(bench.log.join("review_word"), "disagreed").unwrap();
    pull(&bench, 0, "c1");
    bench.tick().await;
    pull(&bench, -10, "c1");
    let run = bench.state(&id, Phase::NeedsAttention).await;
    assert_eq!(run.cause.as_deref(), Some("review_disagreed"));
    assert_eq!(
        run.children[0].rounds[1].outcome.as_deref(),
        Some("disagreed")
    );
}

#[tokio::test]
async fn past_the_limit_of_rounds_the_run_needs_a_person_and_nothing_repeats() {
    let bench = Bench::new().await;
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflows[0].delivery.review_rounds = 1;
    bench.host.state.settings.save_map(&map).unwrap();
    let id = coded(&bench).await;
    pull(&bench, -10, "c1");
    bench.state(&id, Phase::AddressingReview).await;
    pull(&bench, 0, "c2");
    threads(&bench, Some(bench.now() + 5));
    let run = bench.state(&id, Phase::PullRequests).await;
    assert_eq!(
        run.children[0].rounds[0].outcome.as_deref(),
        Some("resubmitted")
    );
    // Rejected again: one round is all this workflow allows.
    pull(&bench, -10, "c2");
    let run = bench.state(&id, Phase::NeedsAttention).await;
    assert_eq!(run.cause.as_deref(), Some("review_limit"));
    assert!(
        run.attention
            .unwrap()
            .contains("sent back for review 1 times")
    );
    assert_eq!(run.children[0].rounds.len(), 1);
    // Resumed, it waits on; the vote that stayed negative starts nothing again.
    bench
        .op("workflow_run_resume", json!({"run": id, "confirmed": true}))
        .await
        .unwrap();
    for _ in 0..3 {
        bench.tick().await;
    }
    assert_eq!(bench.run(&id).state, Phase::PullRequests);
    assert_eq!(bench.count("rounds"), 1);
}

#[tokio::test]
async fn a_session_its_cli_cannot_continue_is_taken_over_by_a_new_one_with_a_handover() {
    let bench = Bench::new().await;
    let id = coded(&bench).await;
    // The CLI cannot continue the session (it exits at once): a new session takes over.
    bench.mode("review_mode", "exit");
    pull(&bench, -10, "c1");
    bench.state(&id, Phase::AddressingReview).await;
    pull(&bench, -10, "c2");
    threads(&bench, Some(bench.now() + 30));
    let run = bench.state(&id, Phase::PullRequests).await;
    let round = &run.children[0].rounds[0];
    assert_eq!((round.n, round.mode.as_deref()), (1, Some("new_session")));
    assert_eq!(
        round.outcome.as_deref(),
        Some("resubmitted"),
        "{:?}",
        round.missing
    );
    assert_eq!(round.requested, vec!["Rita Revisora: comment".to_owned()]);
    let original = run.children[0].coding.clone().unwrap();
    let session = implementation::observe(&bench.host, &bench.machine, &original.id)
        .await
        .unwrap()
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first = fs::read_to_string(bench.log.join("round_args.1")).unwrap();
    assert!(first.contains(&format!("--session={session}")), "{first}");
    let args = fs::read_to_string(bench.log.join("round_args.2")).unwrap();
    assert!(!args.contains(&format!("--session={session}")), "{args}");
    let prompt = fs::read_to_string(bench.log.join("round.2")).unwrap();
    assert!(
        prompt.contains("Handover: an earlier session of this workflow run coded this work item")
    );
    assert!(
        prompt.contains("Implemented change 1."),
        "the end of the first session's report"
    );
}

#[tokio::test]
async fn a_round_interrupted_by_a_restart_of_the_host_starts_again_without_counting_twice() {
    let mut bench = Bench::new().await;
    let id = coded(&bench).await;
    bench.mode("review_mode", "block");
    pull(&bench, -10, "c1");
    bench
        .until(&id, |r| {
            r.children[0]
                .rounds
                .first()
                .is_some_and(|round| round.coding.as_ref().is_some_and(|c| c.started))
        })
        .await;
    while bench.count("rounds") == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    bench.restart().await;
    let run = bench.state(&id, Phase::Interrupted).await;
    assert_eq!(run.stopped_in, Some(Phase::AddressingReview));
    // Resumed: the same round's session again, continuing the session that coded the child.
    let _ = fs::remove_file(bench.log.join("review_mode"));
    pull(&bench, 0, "c2");
    threads(&bench, Some(bench.now() + 30));
    bench
        .op("workflow_run_resume", json!({"run": id, "confirmed": true}))
        .await
        .unwrap();
    let run = bench.state(&id, Phase::PullRequests).await;
    assert_eq!(run.children[0].rounds.len(), 1);
    assert_eq!(
        run.children[0].rounds[0].outcome.as_deref(),
        Some("resubmitted")
    );
    assert_eq!(bench.count("rounds"), 2);
}

#[tokio::test]
async fn a_round_whose_machine_is_gone_needs_a_person_after_half_an_hour() {
    let bench = Bench::new().await;
    let id = coded(&bench).await;
    // The run's child was coded on a machine that is no longer reachable.
    let mut run = bench.run(&id);
    run.children[0].coding.as_mut().unwrap().machine = uuid::Uuid::new_v4().to_string();
    bench.save(&run);
    pull(&bench, -10, "c1");
    let run = bench.state(&id, Phase::AddressingReview).await;
    assert!(run.children[0].rounds[0].coding.is_none());
    bench.offset.fetch_add(
        super::super::super::review::MACHINE_WAIT_SECONDS + 1,
        Ordering::SeqCst,
    );
    let run = bench.state(&id, Phase::NeedsAttention).await;
    assert_eq!(run.cause.as_deref(), Some("review_machine"));
    assert!(run.attention.unwrap().contains("has not answered"));
    // Nothing was launched and the round was not counted twice.
    assert_eq!(bench.count("rounds"), 0);
    assert_eq!(run.children[0].rounds.len(), 1);
}

#[tokio::test]
async fn a_workflow_without_rounds_records_the_votes_and_waits_as_before() {
    let bench = Bench::new().await;
    let mut map = bench.host.state.settings.map().unwrap();
    map.workflows[0].delivery.review_rounds = 0;
    bench.host.state.settings.save_map(&map).unwrap();
    let id = coded(&bench).await;
    pull(&bench, -10, "c1");
    for _ in 0..3 {
        bench.tick().await;
    }
    let run = bench.run(&id);
    assert_eq!(run.state, Phase::PullRequests);
    assert!(run.children[0].rounds.is_empty());
    assert_eq!(run.children[0].votes.get("41:reviewer-1"), Some(&-10));
}

#[tokio::test]
async fn after_a_conflict_the_person_may_have_the_agent_update_its_branch() {
    let bench = Bench::new().await;
    let id = coded(&bench).await;
    // Its pull request conflicts with main.
    let mut conflicted = bench.catalog.clone_pulls(WEB, "feature/child-1");
    conflicted[0].merge_status = Some("conflicts".into());
    conflicted[0].target_commit = Some("t1".into());
    bench
        .catalog
        .set_pulls(WEB, "feature/child-1", conflicted.clone());
    bench.catalog.set_conflicts(41, &["CHANGELOG.md"]);
    bench.state(&id, Phase::NeedsAttention).await;
    // Updating the branch needs confirmation, then the agent's session runs a conflict round.
    let refused = bench
        .op("workflow_run_update_branch", json!({"run": id}))
        .await
        .unwrap_err();
    assert!(
        refused.contains("CHANGELOG.md") && refused.contains("approves nothing"),
        "{refused}"
    );
    bench.mode("review_mode", "block");
    bench
        .op(
            "workflow_run_update_branch",
            json!({"run": id, "confirmed": true}),
        )
        .await
        .unwrap();
    bench
        .until(&id, |r| {
            r.children[0]
                .rounds
                .first()
                .is_some_and(|round| round.coding.as_ref().is_some_and(|c| c.started))
        })
        .await;
    while bench.count("rounds") == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let prompt = fs::read_to_string(bench.log.join("round.1")).unwrap();
    assert!(
        prompt.contains("conflicts with main in CHANGELOG.md"),
        "{prompt}"
    );
    // Pushed and resolved: it waits for the pull request again.
    let mut resolved = conflicted;
    resolved[0].merge_status = Some("succeeded".into());
    resolved[0].source_commit = Some("c2".into());
    bench.catalog.set_pulls(WEB, "feature/child-1", resolved);
    threads(&bench, Some(bench.now() + 5));
    fs::write(bench.log.join("review_release"), "").unwrap();
    let run = bench.state(&id, Phase::PullRequests).await;
    let round = &run.children[0].rounds[0];
    assert_eq!(
        (round.kind.as_str(), round.outcome.as_deref()),
        ("conflict", Some("resubmitted"))
    );
    assert!(run.conflict.is_none());
}
