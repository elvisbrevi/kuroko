//! A coding session's reports through its run's tools (`implementation::report`, what its
//! `delivery_report` and `finding_report` calls reach): its deliveries are its branches, its Wiki
//! pages count once code finds them changed, and its findings are kept once and settled by the
//! person.
use super::*;

async fn report(bench: &Bench, coding: &str, args: Value) -> Result<Value, String> {
    // What the session's tool call reaches: its tool name, then the arguments it gave.
    let mut args = args;
    let kind = args["kind"].as_str().unwrap_or_default().to_owned();
    args.as_object_mut().unwrap().remove("kind");
    let tool = match kind.as_str() {
        "delivery" => "delivery_report",
        "finding" => "finding_report",
        other => other,
    };
    implementation::report(&bench.host.state.implementation.db, coding, tool, &args)
        .map_err(|e| e.to_string())
}

/// The coding run of the run's first child, once it is running.
async fn running(bench: &Bench, id: &str) -> String {
    let run = bench
        .until(id, |r| {
            r.children
                .first()
                .and_then(|c| c.coding.as_ref())
                .is_some_and(|c| c.started && c.state.as_deref() == Some("running"))
        })
        .await;
    run.children[0].coding.clone().unwrap().id
}

#[tokio::test]
async fn reports_are_the_runs_branches_and_findings_kept_once_whatever_its_final_text() {
    let bench = Bench::new().await;
    bench.mode("mode", "block");
    let id = bench.start(TASK).await;
    let coding = running(&bench, &id).await;
    // Its delivery, reported before its final text: the tool's branch is the one taken.
    report(&bench, &coding, json!({"kind": "delivery", "repository": "payments-web", "branch": "feature/from-tool", "pull_request": 7}))
        .await
        .unwrap();
    let found = report(&bench, &coding, json!({"kind": "finding", "title": "Rounding differs from the legacy", "detail": "The legacy rounds half up.", "severity": "important", "evidence": "Rounding.cs:42"}))
        .await
        .unwrap();
    assert_eq!(found["seq"], 2);
    // Invalid reports are refused and change nothing.
    for bad in [
        json!({"kind": "finding", "title": "x", "detail": "y", "severity": "urgent"}),
        json!({"kind": "delivery", "repository": "payments-web", "branch": "a b"}),
        json!({"kind": "delivery", "repository": "https://dev.azure.com/x/_git/web", "branch": "x"}),
        json!({"kind": "delivery"}),
        json!({"kind": "note"}),
    ] {
        assert!(report(&bench, &coding, bad.clone()).await.is_err(), "{bad}");
    }
    // Kept as soon as the engine observes the running session, once.
    bench.tick().await;
    bench.tick().await;
    let listed = bench
        .op("workflow_findings", json!({"run": id}))
        .await
        .unwrap();
    let findings = listed["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{listed}");
    assert_eq!(findings[0]["severity"], "important");
    assert_eq!(findings[0]["state"], "open");
    assert_eq!(findings[0]["item"], TASK);
    bench.release();
    let run = bench.state(&id, Phase::PullRequests).await;
    assert_eq!(run.children[0].branch.as_deref(), Some("feature/from-tool"));
    // A finished run takes no more reports.
    assert!(
        report(
            &bench,
            &coding,
            json!({"kind": "finding", "title": "Late", "detail": "", "severity": "minor"})
        )
        .await
        .is_err()
    );
    // The person settles it, once.
    let finding = findings[0]["id"].as_str().unwrap();
    bench
        .op(
            "workflow_finding_resolve",
            json!({"finding": finding, "note": "Fixed in !7"}),
        )
        .await
        .unwrap();
    let again = bench
        .op("workflow_finding_dismiss", json!({"finding": finding}))
        .await
        .unwrap_err();
    assert!(again.contains("already resolved"), "{again}");
    let open = bench
        .op("workflow_findings", json!({"state": "open"}))
        .await
        .unwrap();
    assert!(open["findings"].as_array().unwrap().is_empty());
    assert!(bench.catalog.only_reads());
}

#[tokio::test]
async fn a_reported_wiki_page_the_user_changed_while_it_coded_is_its_delivery() {
    let bench = Bench::new().await;
    bench.mode("mode", "quiet");
    let id = bench.start(TASK).await;
    let coding = running(&bench, &id).await;
    bench
        .catalog
        .wiki_change("me", "/Procedures/Deploy the portal", bench.now());
    report(
        &bench,
        &coding,
        json!({"kind": "delivery", "wiki_pages": ["Procedures/Deploy-the-portal.md", "/Not/Changed"]}),
    )
    .await
    .unwrap();
    bench.release();
    let run = bench.state(&id, Phase::Done).await;
    let pages: Vec<&str> = run.children[0]
        .wiki_pages
        .iter()
        .map(|p| p.path.as_str())
        .collect();
    assert_eq!(pages, ["/Procedures/Deploy the portal"]);
    assert!(run.children[0].pull_requests.is_empty());
    assert!(bench.catalog.only_reads());
}
