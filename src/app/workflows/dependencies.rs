//! The dependencies of a workflow run's item, and of each child it codes: read from Azure DevOps
//! (their Predecessor links, `ado::dependencies`), judged by code, never by a model, and waited
//! for in `waiting_dependency`, which takes no slot.
//!
//! **When a dependency is met.** The pull requests known for the predecessor (those linked to it
//! in Azure DevOps and those its workflow runs recorded), leaving out abandoned ones, must all be
//! completed into their repository's default branch (or the workflow's target branch): the code
//! the next agent needs is then where it starts from. Only a predecessor without such a pull
//! request is judged by its state, met once the category of that state is `Completed`. So when
//! the two disagree, the pull requests decide: an item closed with its pull request still active
//! is not met, and an item whose pull requests are completed is met while its state still says
//! in progress (PTA changes no state).
//!
//! **What never waits.** A predecessor that does not exist or cannot be read, one of another
//! project or organization, one whose state is `Removed`, and a cycle back to the item stop the
//! run in `needs_attention` with the reason; a wait longer than `dependency_wait_days` does too.
//! The person may skip the wait, for one predecessor or for all (`workflow_run_skip_dependency`).
//!
//! **Delivery settings** (`Workflow::delivery`). With `stack`, a predecessor whose one counted
//! pull request is open and whose branch is known is met by stacking: the child starts from that
//! branch and opens its pull request into it. With `max_open_pull_requests`, a run whose next
//! child would open one more pull request in its repository than the workflow allows waits in
//! `waiting_dependency` too, behind the runs created before it that wait for the same repository.
use super::record::{Run, State as Phase};
use super::*;
use crate::ado::dependencies::{Graph, Node};
use crate::ado::workflow::PullRequest;

/// A pull request that counts for a dependency.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct DependencyPull {
    pub id: u64,
    pub url: String,
    pub status: String,
    /// The branch it merges into, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The branch it merges from, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Its repository's ID, in lowercase.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repository: String,
}

/// The open pull request a child is stacked on (`stack` policy): its branch is where the child
/// starts from and what its own pull request goes into.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Base {
    /// The repository's ID, in lowercase.
    pub repository: String,
    /// The branch name (without `refs/heads/`).
    pub branch: String,
    pub pull_request: u64,
    pub url: String,
}

/// One predecessor of a run's item or child, as the engine last judged it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Dependency {
    pub id: u64,
    /// Its verified link (empty for one that could not be read).
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub state: String,
    /// The category of its state (`Completed`, `Removed`, ...), empty when unknown.
    #[serde(default)]
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_requests: Vec<DependencyPull>,
    pub satisfied: bool,
    /// What decided: `pull_requests`, `state`, `stacked`, `skipped` (by the person) or
    /// `blocked`.
    pub rule: String,
    /// Code's sentence of the fact (`Pull request !7 is active.`).
    pub reason: String,
    /// Met by stacking on its open pull request (`stack` policy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Base>,
}

/// Why and since when a run waits in `waiting_dependency`, and where it goes on.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Wait {
    /// The stage it goes on to once the dependencies are met.
    pub continue_to: Phase,
    /// The work item whose dependencies it waits for (the item, or the child to code).
    pub subject: u64,
    pub since: i64,
    /// When the dependencies were last read.
    pub checked_at: i64,
    /// What it waits for, as code says it.
    pub text: String,
    /// `dependency` (its predecessors), or `open_pull_requests` (the workflow's limit of open
    /// pull requests in `repository`).
    #[serde(
        default = "dependency_reason",
        skip_serializing_if = "is_dependency_reason"
    )]
    pub reason: String,
    /// The repository whose open pull requests it waits for (its lowercase ID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}
fn dependency_reason() -> String {
    "dependency".into()
}
fn is_dependency_reason(reason: &String) -> bool {
    reason == "dependency"
}

/// What the dependencies of one work item allow.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) enum Verdict {
    Met,
    Waiting(String),
    /// Something a person must look at: it would never be met by waiting.
    Blocked(String),
}

/// A pull request a workflow run recorded, with the branch it was read for.
pub(in crate::app) type Known = (PullRequest, Option<String>);

/// The pull requests known for a work item, by its verified link: those its workflow runs
/// recorded (the run's own item, or a child of a run), each with the branch it was read for.
pub(in crate::app) fn known_pulls(runs: &[Run]) -> HashMap<String, Vec<Known>> {
    let mut known: HashMap<String, Vec<Known>> = HashMap::new();
    for run in runs {
        for child in &run.children {
            let delivered = child.delivered();
            let pulls: Vec<Known> = child
                .pull_requests
                .iter()
                .map(|p| {
                    let source = delivered
                        .iter()
                        .find(|(r, _)| r.id.eq_ignore_ascii_case(&p.repository))
                        .map(|(_, b)| b.clone());
                    (p.clone(), source)
                })
                .collect();
            known
                .entry(child.url.clone())
                .or_default()
                .extend(pulls.iter().cloned());
            if child.url != run.item.url && !run.task() {
                known.entry(run.item.url.clone()).or_default().extend(pulls);
            }
        }
    }
    known
}

/// The pull requests the runs of `workflow` have open in `repository` (its lowercase ID), and
/// one for each child coded there now or coded with a branch there whose pull request is not
/// read yet (it opens one): their links.
pub(in crate::app) fn open_pulls(runs: &[Run], workflow: &str, repository: &str) -> Vec<String> {
    let mut open = Vec::new();
    for run in runs
        .iter()
        .filter(|r| !r.state.finished() && r.workflow.as_deref() == Some(workflow))
    {
        // Its next child passed the limit for this repository and is about to be coded there.
        let reserved = run.coding_into.as_deref() == Some(repository)
            && run
                .children
                .iter()
                .find(|c| c.outcome.is_none())
                .is_some_and(|c| c.coding.as_ref().is_none_or(|i| !i.started));
        if reserved {
            open.push(run.item.url.clone());
        }
        for child in &run.children {
            let pulls: Vec<&PullRequest> = child
                .pull_requests
                .iter()
                .filter(|p| p.repository.eq_ignore_ascii_case(repository))
                .collect();
            open.extend(
                pulls
                    .iter()
                    .filter(|p| p.status == "active" && !p.accepted)
                    .map(|p| p.url.clone()),
            );
            // Being coded there now, or coded with a branch there whose pull request is not read
            // yet: it opens one.
            let coding = child.outcome.is_none()
                && child.coding.as_ref().is_some_and(|c| c.started)
                && child
                    .repository
                    .as_ref()
                    .is_some_and(|r| r.id.eq_ignore_ascii_case(repository));
            let delivering = child.outcome.as_deref() == Some("done")
                && child
                    .delivered()
                    .iter()
                    .any(|(r, _)| r.id.eq_ignore_ascii_case(repository));
            if (coding || delivering) && pulls.is_empty() {
                open.push(child.url.clone());
            }
        }
    }
    open
}

/// An older run of the same workflow that waits for the open pull requests of the same
/// repository: it goes first. `runs` is newest first, as the store lists them (creation order,
/// which a timestamp in seconds cannot tell apart).
pub(in crate::app) fn queued_before<'a>(
    runs: &'a [Run],
    run: &Run,
    repository: &str,
) -> Option<&'a Run> {
    let at = runs.iter().position(|r| r.id == run.id).unwrap_or(0);
    runs.iter().skip(at + 1).find(|other| {
        other.workflow == run.workflow
            && other.state == Phase::WaitingDependency
            && other.wait.as_ref().is_some_and(|w| {
                w.reason == "open_pull_requests" && w.repository.as_deref() == Some(repository)
            })
    })
}

/// The pull requests that count for a predecessor: those linked to it, then those its runs
/// recorded that are not linked, leaving out abandoned ones.
fn counted(node: &Node, known: &HashMap<String, Vec<Known>>) -> Vec<(String, DependencyPull)> {
    let mut found: Vec<(String, DependencyPull)> = node
        .pull_requests
        .iter()
        .map(|p| {
            (
                p.repository.to_lowercase(),
                DependencyPull {
                    id: p.id,
                    url: p.url.clone(),
                    status: p.status.clone(),
                    target: (!p.target.is_empty()).then(|| p.target.clone()),
                    source: (!p.source.is_empty()).then(|| branch(&p.source).to_owned()),
                    repository: p.repository.to_lowercase(),
                },
            )
        })
        .collect();
    for (pull, source) in known.get(&node.url).into_iter().flatten() {
        let repository = pull.repository.to_lowercase();
        if !found
            .iter()
            .any(|(r, p)| p.id == pull.id && *r == repository)
        {
            found.push((
                repository.clone(),
                DependencyPull {
                    id: pull.id,
                    url: pull.url.clone(),
                    status: pull.status.clone(),
                    target: pull.target.clone(),
                    source: source.clone(),
                    repository,
                },
            ));
        }
    }
    found.retain(|(_, p)| p.status != "abandoned");
    found
}

/// The branch name of a ref (`refs/heads/develop` → `develop`).
fn branch(reference: &str) -> &str {
    reference.strip_prefix("refs/heads/").unwrap_or(reference)
}

/// A cycle from the item back to itself through predecessors not yet completed nor skipped,
/// as `#A → #B → #A`.
fn cycle(graph: &Graph, skipped: &[u64]) -> Option<String> {
    let open = |id: &u64| {
        !skipped.contains(id)
            && graph
                .nodes
                .get(id)
                .is_some_and(|n| n.problem.is_none() && n.category != "Completed")
    };
    // Depth-first over the open predecessors, keeping the path.
    let mut stack: Vec<(u64, Vec<u64>)> = graph
        .direct
        .iter()
        .filter(|id| open(id))
        .map(|id| (*id, vec![graph.root, *id]))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    while let Some((id, path)) = stack.pop() {
        let Some(node) = graph.nodes.get(&id) else {
            continue;
        };
        for next in &node.predecessors {
            if *next == graph.root {
                let mut path = path.clone();
                path.push(graph.root);
                return Some(
                    path.iter()
                        .map(|id| format!("#{id}"))
                        .collect::<Vec<_>>()
                        .join(" → "),
                );
            }
            if open(next) && seen.insert(*next) {
                let mut path = path.clone();
                path.push(*next);
                stack.push((*next, path));
            }
        }
    }
    None
}

/// Judge the dependencies of `graph.root`, of project `project`: each predecessor met or not
/// with code's reason, and what that allows. `defaults` holds each repository's default branch
/// by lowercase ID; `target` the workflow's own target branch, which replaces it; with `stack`
/// one predecessor whose one counted pull request is open, with its branch known, is met by
/// stacking on it.
pub(in crate::app) fn evaluate(
    graph: &Graph,
    project: &str,
    known: &HashMap<String, Vec<Known>>,
    defaults: &HashMap<String, String>,
    (target, stack): (Option<&str>, bool),
    skipped: &[u64],
) -> (Verdict, Vec<Dependency>) {
    let mut stacked = false;
    let mut blocked = Vec::new();
    for url in &graph.foreign {
        blocked.push(format!(
            "A Predecessor link names a work item of another organization ({url}), which this run cannot read."
        ));
    }
    let mut dependencies = Vec::new();
    for id in &graph.direct {
        let node = graph.nodes.get(id).cloned().unwrap_or(Node {
            id: *id,
            problem: Some("missing".into()),
            ..Default::default()
        });
        let mut dependency = Dependency {
            id: *id,
            url: node.url.clone(),
            title: node.title.clone(),
            kind: node.kind.clone(),
            project: node.project.clone(),
            state: node.state.clone(),
            category: node.category.clone(),
            ..Default::default()
        };
        let pulls = counted(&node, known);
        dependency.pull_requests = pulls.iter().map(|(_, p)| p.clone()).collect();
        let problem = if node.problem.is_some() {
            Some(format!(
                "Predecessor #{id} does not exist or cannot be read with the read credential."
            ))
        } else if !node.project.eq_ignore_ascii_case(project) {
            Some(format!(
                "Predecessor #{id} belongs to project {}, not {project}.",
                node.project
            ))
        } else if node.category == "Removed" {
            Some(format!(
                "Predecessor #{id} was removed (state {}): its work will not be delivered.",
                node.state
            ))
        } else {
            None
        };
        if skipped.contains(id) {
            dependency.satisfied = true;
            dependency.rule = "skipped".into();
            dependency.reason = "The person skipped this dependency.".into();
        } else if let Some(problem) = problem {
            dependency.rule = "blocked".into();
            dependency.reason = problem.clone();
            blocked.push(problem);
        } else if !pulls.is_empty() {
            dependency.rule = "pull_requests".into();
            let pending: Vec<String> = pulls
                .iter()
                .filter_map(|(repository, p)| {
                    let wanted = target
                        .map(str::to_owned)
                        .or_else(|| defaults.get(repository).map(|d| branch(d).to_owned()));
                    if p.status != "completed" {
                        return Some(format!("pull request !{} is {}", p.id, p.status));
                    }
                    match (&p.target, wanted) {
                        (Some(into), Some(wanted)) if branch(into) != wanted => Some(format!(
                            "pull request !{} was completed into {}, not {wanted}",
                            p.id,
                            branch(into)
                        )),
                        _ => None,
                    }
                })
                .collect();
            dependency.satisfied = pending.is_empty();
            // Stacked on its one open pull request, whose branch is known (once per child).
            if !dependency.satisfied
                && stack
                && !stacked
                && let [(repository, pull)] = pulls.as_slice()
                && pull.status == "active"
                && let Some(source) = pull.source.as_deref()
            {
                stacked = true;
                dependency.satisfied = true;
                dependency.rule = "stacked".into();
                dependency.reason = format!(
                    "Stacked on pull request !{} (branch {source}), which is not completed yet.",
                    pull.id
                );
                dependency.base = Some(Base {
                    repository: repository.clone(),
                    branch: source.to_owned(),
                    pull_request: pull.id,
                    url: pull.url.clone(),
                });
                dependencies.push(dependency);
                continue;
            }
            dependency.reason = if pending.is_empty() {
                format!(
                    "Every pull request of #{id} is completed ({}).",
                    pulls
                        .iter()
                        .map(|(_, p)| format!("!{}", p.id))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                let mut text = pending.join("; ");
                if let Some(first) = text.get_mut(..1) {
                    first.make_ascii_uppercase();
                }
                format!("{text}.")
            };
        } else {
            dependency.rule = "state".into();
            dependency.satisfied = node.category == "Completed";
            dependency.reason = if dependency.satisfied {
                format!(
                    "#{id} is {} and has no pull request to wait for.",
                    node.state
                )
            } else {
                format!("#{id} is {} and has no pull request yet.", node.state)
            };
        }
        dependencies.push(dependency);
    }
    if blocked.is_empty()
        && let Some(cycle) = cycle(graph, skipped)
    {
        blocked.push(format!(
            "The dependencies form a cycle ({cycle}): none of them can go first. Remove one of the links, or skip the dependency."
        ));
    }
    if !blocked.is_empty() {
        return (Verdict::Blocked(blocked.join(" ")), dependencies);
    }
    let open: Vec<String> = dependencies
        .iter()
        .filter(|d| !d.satisfied)
        .map(|d| format!("#{} ({})", d.id, d.reason.trim_end_matches('.')))
        .collect();
    if open.is_empty() {
        (Verdict::Met, dependencies)
    } else {
        (
            Verdict::Waiting(format!("Waiting for {}.", open.join("; "))),
            dependencies,
        )
    }
}

/// `workflow_run_skip_dependency {run, dependency?, confirmed}`: the person lets a run go on
/// without waiting for one predecessor (`dependency`, its work item ID) or for every dependency
/// of the work item it waits on. Taken for a run waiting in `waiting_dependency`, or stopped in
/// `needs_attention` by its dependencies.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkipArgs {
    run: String,
    #[serde(default)]
    dependency: Option<u64>,
    #[serde(default)]
    confirmed: bool,
}

pub(super) async fn skip(host: &Arc<Host>, args: &Value) -> Result<Value, String> {
    let args: SkipArgs = serde_json::from_value(args.clone())
        .map_err(|e| invalid(format!("Invalid arguments: {e}")))?;
    runs::require_engine(&host.state)?;
    let found = runs::found(&host.state, &args.run)?;
    if !args.confirmed {
        return Err(format!(
            "[confirmation_required] Skipping the wait lets workflow run {} of {} go on {} although {} not met: its agent may start without code it needs. Confirm to skip.",
            found.id,
            found.item.url,
            if found.state == Phase::NeedsAttention {
                "from the stage it stopped in"
            } else {
                "to its next stage"
            },
            match args.dependency {
                Some(id) => format!("dependency #{id} is"),
                None => "its dependencies are".into(),
            }
        ));
    }
    let state = &host.state.workflows;
    let lock = state.lock(&args.run);
    let _guard = lock.lock().await;
    let mut run = runs::found(&host.state, &args.run)?;
    let now = chrono::Utc::now().timestamp();
    let stopped_by_dependencies = run.state == Phase::NeedsAttention
        && matches!(
            run.cause.as_deref(),
            Some("dependency" | "dependency_timeout")
        );
    if run.state != Phase::WaitingDependency && !stopped_by_dependencies {
        return Err(conflict(format!(
            "Workflow run {} does not wait for its dependencies (it is {}).",
            run.id,
            run.state.word()
        )));
    }
    let subject = run
        .wait
        .as_ref()
        .map(|w| w.subject)
        .or(run.dependencies_for)
        .unwrap_or(run.item.id);
    // A wait for room among the open pull requests is skipped for that child.
    if run
        .wait
        .as_ref()
        .is_some_and(|w| w.reason == "open_pull_requests")
    {
        run.limit_skipped = Some(subject);
    }
    match args.dependency {
        Some(id) => {
            if !run.skipped_dependencies.contains(&id) {
                run.skipped_dependencies.push(id);
            }
            // Judged again at once, with that one left out.
            if let Some(wait) = run.wait.as_mut() {
                wait.checked_at = 0;
            }
        }
        None => run.dependencies_met = Some(subject),
    }
    if run.state == Phase::WaitingDependency && args.dependency.is_none() {
        let next = run
            .wait
            .as_ref()
            .map(|w| w.continue_to)
            .unwrap_or(Phase::Coding);
        run.wait = None;
        run.advance(next, now, "dependencies skipped");
    } else if stopped_by_dependencies {
        let stage = run.stopped_in.unwrap_or(Phase::Coding);
        if stage == Phase::WaitingDependency && args.dependency.is_none() {
            let next = run
                .wait
                .as_ref()
                .map(|w| w.continue_to)
                .unwrap_or(Phase::Coding);
            run.wait = None;
            run.advance(next, now, "dependencies skipped");
        } else {
            if let Some(wait) = run.wait.as_mut() {
                wait.since = now;
                wait.checked_at = 0;
            }
            run.advance(stage, now, "dependencies skipped");
        }
        run.error = None;
    }
    run.updated_at = now;
    state.db.save(&run).map_err(fail)?;
    Ok(json!({"run": run.view(), "skipped": args.dependency.map_or(json!("all"), |id| json!(id))}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ado::dependencies::LinkedPull;

    const REPO: &str = "1a2b3c4d-5b1d-4f7a-9c8e-0f3c6a2e5e6f";

    fn node(id: u64, state: &str, category: &str, predecessors: &[u64]) -> Node {
        Node {
            id,
            url: format!("https://dev.azure.com/example/Payments/_workitems/edit/{id}"),
            title: format!("Item {id}"),
            kind: "Task".into(),
            project: "Payments".into(),
            state: state.into(),
            category: category.into(),
            predecessors: predecessors.to_vec(),
            pull_requests: Vec::new(),
            problem: None,
        }
    }
    fn pull(id: u64, status: &str, target: &str) -> LinkedPull {
        LinkedPull {
            id,
            repository: REPO.into(),
            repository_name: "api".into(),
            status: status.into(),
            target: target.into(),
            source: String::new(),
            url: format!("https://dev.azure.com/example/Payments/_git/api/pullrequest/{id}"),
        }
    }
    fn graph(root: u64, nodes: Vec<Node>) -> Graph {
        let direct = nodes.first().map(|_| vec![nodes[0].id]).unwrap_or_default();
        Graph {
            root,
            direct,
            nodes: nodes.into_iter().map(|n| (n.id, n)).collect(),
            ..Default::default()
        }
    }
    fn defaults() -> HashMap<String, String> {
        HashMap::from([(REPO.to_owned(), "refs/heads/develop".to_owned())])
    }
    fn judge(graph: &Graph) -> (Verdict, Vec<Dependency>) {
        evaluate(
            graph,
            "Payments",
            &HashMap::new(),
            &defaults(),
            (None, false),
            &[],
        )
    }

    #[test]
    fn pull_requests_decide_before_the_state() {
        // Completed state with an active pull request: the code is not there yet.
        let mut done = node(41, "Done", "Completed", &[]);
        done.pull_requests = vec![pull(6119, "active", "refs/heads/develop")];
        let (verdict, deps) = judge(&graph(50, vec![done.clone()]));
        assert!(
            matches!(verdict, Verdict::Waiting(ref t) if t.contains("#41") && t.contains("Pull request !6119 is active")),
            "{verdict:?}"
        );
        assert_eq!(deps[0].rule, "pull_requests");
        // In progress with every pull request completed into the default branch: met.
        let mut active = node(41, "In Progress", "InProgress", &[]);
        active.pull_requests = vec![
            pull(6119, "completed", "refs/heads/develop"),
            pull(6120, "abandoned", "refs/heads/develop"),
        ];
        let (verdict, deps) = judge(&graph(50, vec![active.clone()]));
        assert_eq!(verdict, Verdict::Met);
        assert!(deps[0].satisfied);
        assert_eq!(deps[0].pull_requests.len(), 1);
        // Completed into another branch is not met.
        active.pull_requests = vec![pull(6119, "completed", "refs/heads/feature/x")];
        let (verdict, _) = judge(&graph(50, vec![active.clone()]));
        assert!(
            matches!(verdict, Verdict::Waiting(ref t) if t.contains("not develop")),
            "{verdict:?}"
        );
        // The workflow's target branch replaces the default branch.
        let (verdict, _) = evaluate(
            &graph(50, vec![active]),
            "Payments",
            &HashMap::new(),
            &defaults(),
            (Some("feature/x"), false),
            &[],
        );
        assert_eq!(verdict, Verdict::Met);
        // Without a pull request the state decides.
        let (verdict, deps) = judge(&graph(50, vec![node(41, "Done", "Completed", &[])]));
        assert_eq!(verdict, Verdict::Met);
        assert_eq!(deps[0].rule, "state");
        let (verdict, _) = judge(&graph(50, vec![node(41, "Active", "InProgress", &[])]));
        assert!(matches!(verdict, Verdict::Waiting(_)));
    }

    #[test]
    fn the_pull_requests_a_run_recorded_count_like_linked_ones() {
        let waiting = node(41, "In Progress", "InProgress", &[]);
        let mut known = HashMap::new();
        known.insert(
            waiting.url.clone(),
            vec![(
                PullRequest {
                    id: 7,
                    repository: REPO.into(),
                    status: "active".into(),
                    url: "https://dev.azure.com/example/Payments/_git/api/pullrequest/7".into(),
                    ..Default::default()
                },
                Some("feature/7".to_owned()),
            )],
        );
        let g = graph(50, vec![waiting]);
        let (verdict, _) = evaluate(&g, "Payments", &known, &defaults(), (None, false), &[]);
        assert!(matches!(verdict, Verdict::Waiting(ref t) if t.contains("!7 is active")));
        // With the stack policy the one open pull request, its branch known, is stacked on.
        let (verdict, deps) = evaluate(&g, "Payments", &known, &defaults(), (None, true), &[]);
        assert_eq!(verdict, Verdict::Met);
        assert_eq!(deps[0].rule, "stacked");
        let base = deps[0].base.clone().unwrap();
        assert_eq!((base.branch.as_str(), base.pull_request), ("feature/7", 7));
        known.values_mut().flatten().for_each(|(p, _)| {
            p.status = "completed".into();
            p.target = Some("refs/heads/develop".into());
        });
        let (verdict, _) = evaluate(&g, "Payments", &known, &defaults(), (None, false), &[]);
        assert_eq!(verdict, Verdict::Met);
    }

    #[test]
    fn stacking_takes_one_open_pull_request_with_a_known_branch() {
        // Two open pull requests, or a branch nobody knows: it waits as without the policy.
        let mut two = node(41, "Active", "InProgress", &[]);
        two.pull_requests = vec![
            pull(1, "active", "refs/heads/develop"),
            pull(2, "active", "refs/heads/develop"),
        ];
        let (verdict, _) = evaluate(
            &graph(50, vec![two]),
            "Payments",
            &HashMap::new(),
            &defaults(),
            (None, true),
            &[],
        );
        assert!(matches!(verdict, Verdict::Waiting(_)));
        let mut unknown = node(41, "Active", "InProgress", &[]);
        unknown.pull_requests = vec![pull(1, "active", "refs/heads/develop")];
        let (verdict, _) = evaluate(
            &graph(50, vec![unknown.clone()]),
            "Payments",
            &HashMap::new(),
            &defaults(),
            (None, true),
            &[],
        );
        assert!(matches!(verdict, Verdict::Waiting(_)));
        // Its branch read from the linked pull request: stacked.
        unknown.pull_requests[0].source = "refs/heads/feature/41".into();
        let (verdict, deps) = evaluate(
            &graph(50, vec![unknown]),
            "Payments",
            &HashMap::new(),
            &defaults(),
            (None, true),
            &[],
        );
        assert_eq!(verdict, Verdict::Met);
        assert_eq!(deps[0].base.as_ref().unwrap().branch, "feature/41");
    }

    #[test]
    fn open_pull_requests_of_a_workflow_count_per_repository_and_the_older_waiting_run_goes_first()
    {
        use super::super::record::{Child, Implementation, Item};
        let run = |id: u64, at: i64| {
            let mut run = Run::new(
                Item {
                    id,
                    url: format!("https://dev.azure.com/example/Payments/_workitems/edit/{id}"),
                    ..Default::default()
                },
                at,
            );
            run.workflow = Some("pd".into());
            run
        };
        let repository = crate::workflows::Repository {
            id: REPO.into(),
            name: "api".into(),
        };
        let mut open = run(1, 100);
        open.state = Phase::PullRequests;
        open.children = vec![Child {
            id: 1,
            outcome: Some("done".into()),
            repository: Some(repository.clone()),
            pull_requests: vec![
                PullRequest {
                    id: 6119,
                    repository: REPO.into(),
                    status: "active".into(),
                    url: "pr/6119".into(),
                    ..Default::default()
                },
                PullRequest {
                    id: 6100,
                    repository: REPO.into(),
                    status: "completed".into(),
                    url: "pr/6100".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let mut coding = run(2, 101);
        coding.state = Phase::Coding;
        coding.children = vec![Child {
            id: 2,
            url: "https://dev.azure.com/example/Payments/_workitems/edit/2".into(),
            repository: Some(repository.clone()),
            coding: Some(Implementation {
                started: true,
                ..Default::default()
            }),
            ..Default::default()
        }];
        let mut other = run(3, 102);
        other.workflow = Some("another".into());
        other.state = Phase::PullRequests;
        other.children = open.children.clone();
        let runs = vec![open, coding, other];
        assert_eq!(
            open_pulls(&runs, "pd", REPO),
            vec![
                "pr/6119".to_owned(),
                "https://dev.azure.com/example/Payments/_workitems/edit/2".to_owned()
            ]
        );
        assert!(open_pulls(&runs, "pd", "another-repository").is_empty());
        // Two runs wait for that repository: the older one goes first.
        let waiting = |id: u64, at: i64| {
            let mut r = run(id, at);
            r.state = Phase::WaitingDependency;
            r.wait = Some(Wait {
                continue_to: Phase::Coding,
                subject: id,
                since: at,
                checked_at: at,
                text: String::new(),
                reason: "open_pull_requests".into(),
                repository: Some(REPO.into()),
            });
            r
        };
        // Newest first, as the store lists them; created in the same second.
        let runs = vec![waiting(5, 200), waiting(4, 200)];
        assert_eq!(
            queued_before(&runs, &runs[0], REPO).map(|r| r.item.id),
            Some(4)
        );
        assert!(queued_before(&runs, &runs[1], REPO).is_none());
    }

    #[test]
    fn what_waiting_cannot_fix_is_blocked_with_its_reason() {
        let mut missing = node(41, "", "", &[]);
        missing.problem = Some("missing".into());
        let (verdict, _) = judge(&graph(50, vec![missing]));
        assert!(matches!(verdict, Verdict::Blocked(ref t) if t.contains("#41 does not exist")));
        let mut other = node(41, "Active", "InProgress", &[]);
        other.project = "Billing".into();
        let (verdict, _) = judge(&graph(50, vec![other]));
        assert!(matches!(verdict, Verdict::Blocked(ref t) if t.contains("project Billing")));
        let (verdict, _) = judge(&graph(50, vec![node(41, "Removed", "Removed", &[])]));
        assert!(matches!(verdict, Verdict::Blocked(ref t) if t.contains("removed")));
        let mut foreign = graph(50, vec![]);
        foreign.foreign = vec!["https://dev.azure.com/other/_apis/wit/workItems/9".into()];
        let (verdict, _) = judge(&foreign);
        assert!(matches!(verdict, Verdict::Blocked(ref t) if t.contains("another organization")));
        // A skipped predecessor no longer counts, even a blocked one.
        let (verdict, deps) = evaluate(
            &graph(50, vec![node(41, "Removed", "Removed", &[])]),
            "Payments",
            &HashMap::new(),
            &defaults(),
            (None, false),
            &[41],
        );
        assert_eq!(verdict, Verdict::Met);
        assert_eq!(deps[0].rule, "skipped");
    }

    #[test]
    fn a_cycle_back_to_the_item_is_described_and_a_completed_link_breaks_it() {
        // 50 needs 41, which needs 42, which needs 50.
        let g = graph(
            50,
            vec![
                node(41, "Active", "InProgress", &[42]),
                node(42, "Active", "InProgress", &[50]),
            ],
        );
        let (verdict, _) = judge(&g);
        assert!(
            matches!(verdict, Verdict::Blocked(ref t) if t.contains("#50 → #41 → #42 → #50")),
            "{verdict:?}"
        );
        // A → B → A.
        let g = graph(50, vec![node(41, "Active", "InProgress", &[50])]);
        assert!(matches!(judge(&g).0, Verdict::Blocked(ref t) if t.contains("#50 → #41 → #50")));
        // A completed link in the middle breaks it: no deadlock, it is waited for normally.
        let g = graph(
            50,
            vec![
                node(41, "Active", "InProgress", &[42]),
                node(42, "Done", "Completed", &[50]),
            ],
        );
        assert!(matches!(judge(&g).0, Verdict::Waiting(_)));
    }
}
