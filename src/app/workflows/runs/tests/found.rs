//! What a coding run delivered without reporting it, found from facts: the branches the user
//! pushed to the run's repositories since it started, the pull requests of those branches and
//! those linked to the item, and the pages the user changed in the project's Wiki. They are
//! named to the person and become the delivery only when the person accepts them.
use super::*;

const OTHER: &str = "9e8d7c6b-5a4f-4e3d-8c2b-1a0f9e8d7c6b";

fn found(run: &Run) -> record::Found {
    run.children[0]
        .found
        .clone()
        .expect("what was found is recorded")
}

#[tokio::test]
async fn a_report_without_its_branch_names_what_was_pushed_and_accepting_it_delivers_the_child() {
    let bench = Bench::new().await;
    bench.mode("mode", "nobranch");
    let id = bench.start(TASK).await;
    let at = bench.now();
    bench.catalog.push("me", WEB, "feature/rounding", at);
    // Another person's push, and a push to a repository outside the workflow, are not the run's.
    bench.catalog.push("someone", WEB, "feature/theirs", at);
    bench.catalog.push("me", OTHER, "feature/elsewhere", at);
    // What a pull request merges into is never a delivery.
    bench.catalog.push("me", WEB, "main", at);
    bench.pulls(WEB, "payments-web", "feature/rounding", 7, "active");
    bench.pulls(OTHER, "billing", "feature/elsewhere", 8, "active");
    let run = bench.state(&id, Phase::NeedsAttention).await;
    let attention = run.attention.clone().unwrap();
    assert!(attention.contains("reported no branch"), "{attention}");
    assert!(
        attention.contains("Found, not reported, for https://dev.azure.com/example/Payments/_workitems/edit/200: branch feature/rounding in payments-web (pushed) with pull request https://dev.azure.com/example/Payments/_git/payments-web/pullrequest/7 (active)"),
        "{attention}"
    );
    let found = found(&run);
    assert_eq!(
        found
            .branches
            .iter()
            .map(|b| (b.repository.name.as_str(), b.branch.as_str(), b.linked))
            .collect::<Vec<_>>(),
        [("payments-web", "feature/rounding", false)]
    );
    assert_eq!(
        found.pull_requests.iter().map(|p| p.id).collect::<Vec<_>>(),
        [7]
    );
    assert!(found.wiki_pages.is_empty());
    // A push after the child's coding ended is not its own, whenever it is looked for again.
    bench
        .catalog
        .push("me", WEB, "feature/later", bench.now() + 7200);
    bench.offset.store(7300, Ordering::SeqCst);
    bench
        .op("workflow_run_find", json!({"run": id}))
        .await
        .unwrap();
    let run = bench.run(&id);
    let again = run.children[0].found.clone().unwrap();
    assert_eq!(again.branches.len(), 1, "{:?}", again.branches);
    bench.offset.store(0, Ordering::SeqCst);
    // The confirmation names what it takes, from the run as recorded.
    let asked = crate::app::workflows::found::describe_accept(
        &json!({"run": run.view()}),
        &json!({"run": id}),
    )
    .unwrap();
    assert!(
        asked.contains("branch feature/rounding in payments-web"),
        "{asked}"
    );
    // The person confirms; without it nothing changes.
    let refused = bench
        .op("workflow_run_accept_found", json!({"run": id}))
        .await
        .unwrap_err();
    assert!(refused.contains("Confirm with the user"), "{refused}");
    assert!(
        refused.contains("branch feature/rounding in payments-web"),
        "{refused}"
    );
    assert_eq!(bench.run(&id).state, Phase::NeedsAttention);
    let reply = bench
        .op(
            "workflow_run_accept_found",
            json!({"run": id, "confirmed": true}),
        )
        .await
        .unwrap();
    assert_eq!(
        reply["accepted"],
        json!(["branch feature/rounding in payments-web"])
    );
    let run = bench.run(&id);
    assert_eq!(run.state, Phase::PullRequests);
    assert_eq!(run.children[0].branch.as_deref(), Some("feature/rounding"));
    assert_eq!(run.children[0].outcome.as_deref(), Some("done"));
    bench.pulls(WEB, "payments-web", "feature/rounding", 7, "completed");
    let run = bench.state(&id, Phase::Done).await;
    assert_eq!(run.children[0].pull_requests[0].id, 7);
    // The coding run is never started again, and nothing was written.
    assert_eq!(bench.count("starts"), 1);
    assert!(bench.catalog.only_reads());
}

#[tokio::test]
async fn a_pull_request_linked_to_the_item_is_found_whenever_its_branch_was_pushed() {
    let bench = Bench::new().await;
    bench.mode("mode", "nobranch");
    // Pushed before the run started: no push lists it, the item's link does.
    bench.pulls(WEB, "payments-web", "feature/linked", 9, "active");
    bench.catalog.relate(
        TASK,
        "Pull Request",
        &format!("https://dev.azure.com/example/3c1d2e4f-0000-4000-8000-000000000001/_git/{WEB}/pullrequest/9"),
    );
    // A link to a repository outside the workflow is not followed.
    bench.catalog.relate(
        TASK,
        "Pull Request",
        &format!("https://dev.azure.com/example/3c1d2e4f-0000-4000-8000-000000000001/_git/{OTHER}/pullrequest/4"),
    );
    let id = bench.start(TASK).await;
    let run = bench.state(&id, Phase::NeedsAttention).await;
    let found = found(&run);
    assert_eq!(
        found
            .branches
            .iter()
            .map(|b| (b.branch.as_str(), b.linked, b.pushed_at.is_some()))
            .collect::<Vec<_>>(),
        [("feature/linked", true, false)]
    );
    assert_eq!(
        found.pull_requests.iter().map(|p| p.id).collect::<Vec<_>>(),
        [9]
    );
    assert_eq!(
        found.branches[0].url,
        "https://dev.azure.com/example/Payments/_git/payments-web?version=GBfeature%2Flinked"
    );
    assert_eq!(bench.reads(&format!("GET pullrequest {OTHER}")), 0);
    bench
        .op(
            "workflow_run_accept_found",
            json!({"run": id, "pull_requests": [9], "confirmed": true}),
        )
        .await
        .unwrap();
    let run = bench.run(&id);
    assert_eq!(run.children[0].branch.as_deref(), Some("feature/linked"));
    assert!(bench.catalog.only_reads());
}

#[tokio::test]
async fn a_child_that_delivered_only_a_wiki_page_finishes_without_pull_requests() {
    let bench = Bench::new().await;
    bench.mode("mode", "nobranch");
    let id = bench.start(TASK).await;
    let at = bench.now();
    bench
        .catalog
        .wiki_change("me", "/Procedures/Deploy the portal", at);
    bench
        .catalog
        .wiki_change("someone", "/Procedures/Not mine", at);
    let run = bench.state(&id, Phase::NeedsAttention).await;
    let attention = run.attention.clone().unwrap();
    assert!(
        attention.contains("Wiki page /Procedures/Deploy the portal"),
        "{attention}"
    );
    assert!(!attention.contains("Not mine"), "{attention}");
    bench
        .op(
            "workflow_run_accept_found",
            json!({"run": id, "wiki_pages": ["/Procedures/Deploy the portal"], "confirmed": true}),
        )
        .await
        .unwrap();
    let run = bench.state(&id, Phase::Done).await;
    assert_eq!(run.children[0].wiki_pages.len(), 1);
    assert!(run.children[0].pull_requests.is_empty());
    assert!(stages(&run).contains(&"blocks"));
    assert!(bench.catalog.only_reads());
}

#[tokio::test]
async fn a_failed_coding_run_names_what_it_pushed_and_what_is_gone_is_refused() {
    let bench = Bench::new().await;
    bench.mode("mode", "fail");
    let id = bench.start(TASK).await;
    bench.catalog.push("me", WEB, "feature/half", bench.now());
    let run = bench.state(&id, Phase::NeedsAttention).await;
    let attention = run.attention.clone().unwrap();
    assert!(attention.contains("ended needs_attention"), "{attention}");
    assert!(
        attention.contains("branch feature/half in payments-web (pushed)"),
        "{attention}"
    );
    // Only a run stopped while coding or waiting for its pull requests takes it.
    let mut elsewhere = bench.run(&id);
    elsewhere.stopped_in = Some(Phase::AddressingReview);
    bench.save(&elsewhere);
    let refused = bench
        .op(
            "workflow_run_accept_found",
            json!({"run": id, "confirmed": true}),
        )
        .await
        .unwrap_err();
    assert!(refused.contains("resume it instead"), "{refused}");
    bench.save(&run);
    // Nothing is taken that was never shown.
    let mut unseen = run.clone();
    unseen.children[0].found = None;
    bench.save(&unseen);
    let refused = bench
        .op(
            "workflow_run_accept_found",
            json!({"run": id, "confirmed": true}),
        )
        .await
        .unwrap_err();
    assert!(refused.contains("look first"), "{refused}");
    bench.save(&run);
    // Gone before the person accepts it: refused, and the run is as it was.
    bench.catalog.clear_pushes();
    let refused = bench
        .op(
            "workflow_run_accept_found",
            json!({"run": id, "branches": [{"repository": "payments-web", "branch": "feature/half"}], "confirmed": true}),
        )
        .await
        .unwrap_err();
    assert!(refused.contains("is not found now"), "{refused}");
    let after = bench.run(&id);
    assert_eq!(after.state, Phase::NeedsAttention);
    assert!(after.children[0].outcome.is_none());
    // Looking again records what is found now: nothing.
    let reply = bench
        .op("workflow_run_find", json!({"run": id}))
        .await
        .unwrap();
    assert_eq!(reply["found"][0]["found"]["branches"], json!(null));
    assert!(
        bench.run(&id).children[0]
            .found
            .as_ref()
            .unwrap()
            .branches
            .is_empty()
    );
    assert!(bench.catalog.only_reads());
}

#[tokio::test]
async fn a_branch_another_run_delivered_is_never_found_for_this_one() {
    let bench = Bench::new().await;
    // A first run delivers feature/shared.
    let first = bench.start(TASK).await;
    let mut done = bench.state(&first, Phase::PullRequests).await;
    done.children[0].branch = Some("feature/shared".into());
    bench.save(&done);
    bench
        .op(
            "workflow_run_cancel",
            json!({"run": first, "confirmed": true}),
        )
        .await
        .unwrap();
    bench.mode("mode", "nobranch");
    let id = bench.start(TASK).await;
    bench.catalog.push("me", WEB, "feature/shared", bench.now());
    let run = bench.state(&id, Phase::NeedsAttention).await;
    assert!(found(&run).branches.is_empty());
    assert!(!run.attention.unwrap().contains("Found, not reported"));
}

#[tokio::test]
async fn two_branches_in_one_repository_are_never_taken_together() {
    let bench = Bench::new().await;
    bench.mode("mode", "nobranch");
    let id = bench.start(TASK).await;
    let at = bench.now();
    bench.catalog.push("me", WEB, "feature/one", at);
    bench.catalog.push("me", WEB, "feature/two", at);
    bench.state(&id, Phase::NeedsAttention).await;
    let refused = bench
        .op(
            "workflow_run_accept_found",
            json!({"run": id, "confirmed": true}),
        )
        .await
        .unwrap_err();
    assert!(
        refused.contains("Choose one branch of repository payments-web: feature/one, feature/two"),
        "{refused}"
    );
    bench
        .op(
            "workflow_run_accept_found",
            json!({"run": id, "branches": [{"repository": WEB, "branch": "feature/two"}], "confirmed": true}),
        )
        .await
        .unwrap();
    let run = bench.run(&id);
    assert_eq!(run.children[0].branch.as_deref(), Some("feature/two"));
    assert!(run.children[0].branches.is_empty());
}
