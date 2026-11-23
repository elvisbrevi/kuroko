//! What a coding run delivered without reporting it, found from facts rather than from its
//! final report: the branches the user pushed to the repositories the run was given since it
//! started, the pull requests of those branches and the ones linked to its work item, and the
//! pages the user changed in the project's Wiki. Everything is read with the enabled source's
//! read credential and has its verified link; nothing is written to a provider.
//!
//! The window is the child's coding: from a minute before it started to a minute after it ended,
//! fixed by the first read. What is found is a candidate: it is recorded on the child
//! (`Child::found`) and named in what the person reads, and it becomes the child's delivery only
//! when the person accepts it (`workflow_run_accept_found`), which takes only what was recorded
//! and is still found when read again. `workflow_run_find` reads it again without accepting
//! anything. A branch or page another run already delivered is never a candidate.
use super::engine::Azure;
use super::record::{Child, Found, FoundBranch, RepositoryBranch, Run, State as Phase};
use super::runs::{found as get_run, require_engine};
use super::*;
use crate::ado::workflow::{PullRequest, WikiPage};
use crate::workflows::Repository;
use std::collections::BTreeSet;

/// The window reaches this far before the child's coding started and after it ended.
const GRACE_SECONDS: i64 = 60;
/// At most this many pull requests linked to a work item are read.
const LINKS: usize = 10;
/// The runs looked at for what another run already delivered.
const CLAIMS: i64 = 200;
/// All the reads of one look, at most: past it what was read is kept, marked partial.
const READ_SECONDS: u64 = 90;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The repositories a child's coding run was given: a task's every repository of its workflow;
/// a planned child's its own (every one while it was not placed).
fn given(run: &Run, child: &Child, workflow: &crate::workflows::Workflow) -> Vec<Repository> {
    match &child.repository {
        Some(own) if !run.task() => vec![own.clone()],
        _ => workflow.repositories.clone(),
    }
}

/// Pairs other children already delivered: (`repository id`, `branch`) or (link, time).
type Claims = BTreeSet<(String, String)>;

/// The branches (`repository id`, `branch`) and Wiki page changes (link, time) other children
/// already delivered: those of every other run, and of this run's other children.
// shortcut: a branch another run pushed in the same repository at the same time and has not
// reported yet is listed too (every agent pushes as the user); directory holds keep two runs
// out of one checkout, and the person accepts.
fn claimed(state: &DesktopState, run: &Run, index: usize) -> (Claims, Claims) {
    let mut branches = BTreeSet::new();
    let mut pages = BTreeSet::new();
    let others = state
        .workflows
        .db
        .list(&[], None, CLAIMS)
        .unwrap_or_default();
    let children = others
        .iter()
        .filter(|r| r.id != run.id)
        .flat_map(|r| r.children.iter())
        .chain(
            run.children
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != index)
                .map(|(_, c)| c),
        );
    for child in children {
        for (repository, branch) in child.delivered() {
            branches.insert((repository.id.to_lowercase(), branch));
        }
        for page in &child.wiki_pages {
            pages.insert((page.url.clone(), page.changed_at.clone()));
        }
    }
    (branches, pages)
}

/// The link of a workflow repository at a branch, built from the run's organization and project
/// and the repository's catalog name.
fn branch_link(run: &Run, repository: &Repository, branch: &str) -> Option<String> {
    let mut url = url::Url::parse(&run.item.organization).ok()?;
    {
        let mut path = url.path_segments_mut().ok()?;
        path.pop_if_empty();
        path.push(&run.item.project);
        path.push("_git");
        path.push(&repository.name);
    }
    url.query_pairs_mut()
        .append_pair("version", &format!("GB{branch}"));
    crate::tools::todo::https_link(url.as_str())
}

/// The workflow repository a link names (`…/_git/REPOSITORY/…`, by catalog ID or name).
fn linked_repository<'a>(
    link: &url::Url,
    repositories: &'a [Repository],
) -> Option<&'a Repository> {
    let segments: Vec<String> = link
        .path_segments()?
        .map(|s| {
            url::form_urlencoded::parse(format!("s={}", s.replace('+', "%2B")).as_bytes())
                .next()
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        })
        .collect();
    let at = segments.iter().position(|s| s == "_git")?;
    let named = segments.get(at + 1)?;
    repositories
        .iter()
        .find(|r| r.id.eq_ignore_ascii_case(named) || r.name.eq_ignore_ascii_case(named))
}

/// The pull requests (repository, ID) the work item links, only of `repositories`.
fn linked_pulls(
    detail: &crate::tools::todo::Detail,
    repositories: &[Repository],
) -> Vec<(Repository, u64)> {
    detail
        .relations
        .iter()
        .take(200)
        .filter(|r| r.name == "Pull Request")
        .filter_map(|relation| {
            let link = url::Url::parse(relation.url.as_deref()?).ok()?;
            let repository = linked_repository(&link, repositories)?;
            let number = link
                .path_segments()?
                .next_back()?
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)?;
            Some((repository.clone(), number))
        })
        .take(LINKS)
        .collect()
}

/// Read what child `index` of `run` delivered without reporting it, within the reads' deadline.
pub(in crate::app) async fn discover(
    host: &Arc<Host>,
    azure: &Azure,
    run: &Run,
    index: usize,
    now: i64,
) -> Found {
    let child = run.children.get(index);
    let start = child.and_then(|c| c.started_at).unwrap_or(run.created_at) - GRACE_SECONDS;
    // The first read fixes the window's end: a later look never reaches past the child's coding.
    let until = child
        .and_then(|c| c.found.as_ref().map(|f| f.until))
        .or_else(|| {
            child
                .and_then(|c| c.finished_at)
                .map(|at| at + GRACE_SECONDS)
        })
        .unwrap_or(now + GRACE_SECONDS)
        .min(now.max(start));
    let mut found = Found {
        read_at: now,
        until,
        partial: true,
        ..Default::default()
    };
    if let Ok(read) = tokio::time::timeout(
        std::time::Duration::from_secs(READ_SECONDS),
        read(host, azure, run, index, (start, until), &mut found),
    )
    .await
    {
        found.partial = read;
    }
    found
}

/// The reads of `discover`, filling `found` as they answer; whether one failed.
async fn read(
    host: &Arc<Host>,
    azure: &Azure,
    run: &Run,
    index: usize,
    (start, until): (i64, i64),
    found: &mut Found,
) -> bool {
    let (Some(workflow), Some(child)) = (run.definition.as_ref(), run.children.get(index)) else {
        return true;
    };
    let state = &host.state;
    let window = (
        chrono::DateTime::from_timestamp(start, 0).unwrap_or_default(),
        chrono::DateTime::from_timestamp(until, 0).unwrap_or_default(),
    );
    let mut partial = false;
    let (taken, taken_pages) = claimed(state, run, index);
    // What a pull request merges into is never a delivery: the repository's default branch and
    // the workflow's target, or the usual ones when the defaults cannot be read.
    let defaults = match azure.default_branches(state, run).await {
        Ok(defaults) => defaults,
        Err(_) => {
            partial = true;
            Default::default()
        }
    };
    let target = workflow.delivery.target_branch.trim();
    let free = |repository: &Repository, branch: &str| {
        let default = defaults
            .get(&repository.id.to_lowercase())
            .map(|d| d.trim_start_matches("refs/heads/"));
        !taken.contains(&(repository.id.to_lowercase(), branch.to_owned()))
            && branch != target
            && match default {
                Some(default) => default != branch,
                None => !matches!(branch, "main" | "master"),
            }
    };
    let repositories = given(run, child, workflow);
    for repository in &repositories {
        match azure.pushed_branches(state, run, repository, window).await {
            Ok((pushed, full)) => {
                partial |= full;
                for push in pushed {
                    if free(repository, &push.branch) {
                        found.branches.push(FoundBranch {
                            repository: repository.clone(),
                            branch: push.branch,
                            url: push.url,
                            pushed_at: Some(push.pushed_at),
                            linked: false,
                        });
                    }
                }
            }
            Err(_) => partial = true,
        }
    }
    // The pull requests the work item links that the user opened in the window, of the
    // repositories the child was given, whenever their branch was pushed.
    match azure
        .item(
            state,
            run.item.source.as_deref(),
            Some(&run.item.project),
            child.id,
        )
        .await
    {
        Ok(detail) => {
            for (repository, number) in linked_pulls(&detail, &repositories) {
                match azure
                    .pull_request(state, run, &repository, number, window.0)
                    .await
                {
                    Ok(Some(pull)) => {
                        let Some(branch) = pull.source.clone().filter(|b| free(&repository, b))
                        else {
                            continue;
                        };
                        mark(found, run, &repository, &branch);
                        add_pull(found, pull);
                    }
                    Ok(None) => {}
                    Err(_) => partial = true,
                }
            }
        }
        Err(_) => partial = true,
    }
    // The pull requests of every branch found.
    for candidate in found.branches.clone() {
        match azure
            .branch_pulls(state, run, &candidate.repository, &candidate.branch)
            .await
        {
            Ok(pulls) => {
                for pull in pulls {
                    add_pull(
                        found,
                        PullRequest {
                            source: Some(candidate.branch.clone()),
                            ..pull
                        },
                    );
                }
            }
            Err(_) => partial = true,
        }
    }
    match azure.wiki_pages(state, run, window).await {
        Ok((pages, incomplete)) => {
            partial |= incomplete;
            found.wiki_pages.extend(
                pages
                    .into_iter()
                    .filter(|p| !taken_pages.contains(&(p.url.clone(), p.changed_at.clone()))),
            );
        }
        Err(_) => partial = true,
    }
    partial
}

/// Mark a branch the work item links as found (adding it when no push listed it).
fn mark(found: &mut Found, run: &Run, repository: &Repository, branch: &str) {
    if let Some(known) = found
        .branches
        .iter_mut()
        .find(|b| b.repository.id.eq_ignore_ascii_case(&repository.id) && b.branch == branch)
    {
        known.linked = true;
        return;
    }
    if let Some(url) = branch_link(run, repository, branch) {
        found.branches.push(FoundBranch {
            repository: repository.clone(),
            branch: branch.to_owned(),
            url,
            pushed_at: None,
            linked: true,
        });
    }
}

fn add_pull(found: &mut Found, pull: PullRequest) {
    if !found
        .pull_requests
        .iter()
        .any(|p| p.id == pull.id && p.repository.eq_ignore_ascii_case(&pull.repository))
    {
        found.pull_requests.push(pull);
    }
}

/// What was found for `child` (its link), as one English sentence for the attention text
/// (empty when nothing was).
pub(in crate::app) fn describe(child: &str, found: &Found) -> String {
    let mut parts = Vec::new();
    for branch in &found.branches {
        let pulls: Vec<String> = found
            .pull_requests
            .iter()
            .filter(|p| {
                p.repository.eq_ignore_ascii_case(&branch.repository.id)
                    && p.source.as_deref() == Some(branch.branch.as_str())
            })
            .map(|p| format!("pull request {} ({})", p.url, p.status))
            .collect();
        let with = if pulls.is_empty() {
            String::new()
        } else {
            format!(" with {}", pulls.join(", "))
        };
        let how = if branch.linked {
            "linked to the work item"
        } else {
            "pushed"
        };
        parts.push(format!(
            "branch {} in {} ({how}){with}",
            branch.branch, branch.repository.name
        ));
    }
    for page in &found.wiki_pages {
        parts.push(format!("Wiki page {} ({})", page.path, page.url));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(
        " Found, not reported, for {child}: {}. Accept it as the delivery with workflow_run_accept_found if it is the work, or resume.",
        parts.join("; ")
    )
}

/// The children whose delivery is looked for: coded (their coding run started and is not
/// running) and without a reported or accepted delivery.
fn without_delivery(run: &Run) -> Vec<usize> {
    run.children
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            !c.reported()
                && c.coding.as_ref().is_some_and(|coding| {
                    coding.started && coding.state.as_deref() != Some("running")
                })
        })
        .map(|(i, _)| i)
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    run: String,
    #[serde(default)]
    child: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchChoice {
    repository: String,
    branch: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptArgs {
    run: String,
    #[serde(default)]
    child: Option<u64>,
    #[serde(default)]
    branches: Option<Vec<BranchChoice>>,
    #[serde(default)]
    pull_requests: Option<Vec<u64>>,
    #[serde(default)]
    wiki_pages: Option<Vec<String>>,
    #[serde(default)]
    confirmed: bool,
}

pub(in crate::app) async fn operation(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
    azure: &Azure,
) -> Result<Value, String> {
    match method {
        "workflow_run_find" => find(host, parse(args)?, azure).await,
        "workflow_run_accept_found" => accept(host, parse(args)?, azure).await,
        _ => Err(invalid("Unknown workflow operation.")),
    }
}

fn child_index(run: &Run, child: Option<u64>, candidates: &[usize]) -> Result<usize, String> {
    match child {
        Some(id) => run.children.iter().position(|c| c.id == id).ok_or_else(|| {
            invalid(format!(
                "Work item {id} is not a child of workflow run {}.",
                run.id
            ))
        }),
        None => candidates.first().copied().ok_or_else(|| {
            conflict(format!(
                "Workflow run {} has no coded child without a delivery.",
                run.id
            ))
        }),
    }
}

/// Read again what the run's coded children without a delivery delivered, and record it.
async fn find(host: &Arc<Host>, args: FindArgs, azure: &Azure) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let lock = state.workflows.lock(&args.run);
    let _guard = lock.lock().await;
    let mut run = get_run(state, &args.run)?;
    if run.state.finished() {
        return Err(conflict(format!(
            "Workflow run {} is {}: nothing is looked for in a finished run.",
            run.id,
            run.state.word()
        )));
    }
    let candidates = without_delivery(&run);
    let indexes = match args.child {
        Some(_) => vec![child_index(&run, args.child, &candidates)?],
        None => candidates,
    };
    let at = now();
    let mut listed = Vec::new();
    for index in indexes {
        let found = discover(host, azure, &run, index, at).await;
        listed.push(json!({"child": run.children[index].url, "found": found}));
        run.children[index].found = Some(found);
    }
    run.updated_at = at;
    state.workflows.db.save(&run).map_err(fail)?;
    Ok(json!({"run": run.view(), "found": listed}))
}

/// Accept what was found as a child's delivery, read again now, and let the run go on.
async fn accept(host: &Arc<Host>, args: AcceptArgs, azure: &Azure) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    // One acceptance at a time on the account, so two runs never take the same branch at once.
    let accepting = state.workflows.lock("accept-found");
    let _accepting = accepting.lock().await;
    let lock = state.workflows.lock(&args.run);
    let _guard = lock.lock().await;
    let mut run = get_run(state, &args.run)?;
    if !run.state.stopped() || !matches!(run.stopped_in, Some(Phase::Coding | Phase::PullRequests))
    {
        return Err(conflict(format!(
            "Workflow run {} is {}; what was found is accepted only for a run stopped while coding or waiting for its pull requests: resume it instead.",
            run.id,
            run.state.word()
        )));
    }
    let index = child_index(&run, args.child, &without_delivery(&run))?;
    let child = &run.children[index];
    if child.reported() {
        return Err(conflict(format!(
            "{} already has its delivery; resume the run instead.",
            child.url
        )));
    }
    if !child
        .coding
        .as_ref()
        .is_some_and(|c| c.started && c.state.as_deref() != Some("running"))
    {
        return Err(conflict(format!(
            "The coding run of {} has not ended; wait for it.",
            child.url
        )));
    }
    // Only what was recorded, and so shown, and is still found now.
    let Some(shown) = child.found.clone() else {
        return Err(conflict(format!(
            "Nothing was looked for {} yet: look first (workflow_run_find), then accept what it lists.",
            child.url
        )));
    };
    let at = now();
    let fresh = discover(host, azure, &run, index, at).await;
    let still = |b: &FoundBranch| {
        shown.branches.iter().any(|s| same_branch(s, b))
            && fresh.branches.iter().any(|f| same_branch(f, b))
    };
    let everything =
        args.branches.is_none() && args.pull_requests.is_none() && args.wiki_pages.is_none();
    let gone = |what: String| {
        conflict(format!(
            "{what} is not found now: it is not among what was found for {} and still there.",
            run.children[index].url
        ))
    };
    // The branches chosen, and the source branch of each pull request chosen.
    let mut branches: Vec<FoundBranch> = Vec::new();
    let mut take = |candidate: &FoundBranch| {
        if !branches.iter().any(|b| same_branch(b, candidate)) {
            branches.push(candidate.clone());
        }
    };
    if everything {
        shown
            .branches
            .iter()
            .filter(|b| still(b))
            .for_each(&mut take);
    }
    for choice in args.branches.iter().flatten() {
        let candidate = shown
            .branches
            .iter()
            .find(|b| {
                b.branch == choice.branch
                    && (b.repository.id.eq_ignore_ascii_case(&choice.repository)
                        || b.repository.name.eq_ignore_ascii_case(&choice.repository))
                    && still(b)
            })
            .ok_or_else(|| {
                gone(format!(
                    "Branch {} of repository {}",
                    choice.branch, choice.repository
                ))
            })?;
        take(candidate);
    }
    for number in args.pull_requests.iter().flatten() {
        let candidate = shown
            .pull_requests
            .iter()
            .find(|p| p.id == *number)
            .and_then(|pull| {
                shown.branches.iter().find(|b| {
                    b.repository.id.eq_ignore_ascii_case(&pull.repository)
                        && pull.source.as_deref() == Some(b.branch.as_str())
                })
            })
            .filter(|b| still(b))
            .ok_or_else(|| gone(format!("Pull request {number}")))?;
        take(candidate);
    }
    let fresh_page = |p: &WikiPage| fresh.wiki_pages.iter().any(|f| f.url == p.url);
    let pages: Vec<WikiPage> = match &args.wiki_pages {
        None if everything => shown
            .wiki_pages
            .iter()
            .filter(|p| fresh_page(p))
            .cloned()
            .collect(),
        None => Vec::new(),
        Some(wanted) => wanted
            .iter()
            .map(|w| {
                shown
                    .wiki_pages
                    .iter()
                    .find(|p| (p.path == *w || p.url == *w) && fresh_page(p))
                    .cloned()
                    .ok_or_else(|| gone(format!("Wiki page {w}")))
            })
            .collect::<Result<_, _>>()?,
    };
    let child = &run.children[index];
    if branches.is_empty() && pages.is_empty() {
        return Err(conflict(format!(
            "Nothing that was found for {} is still there.{}",
            child.url,
            if fresh.partial {
                " Some reads failed: look again later."
            } else {
                ""
            }
        )));
    }
    // One branch per repository, as a report gives them.
    for branch in &branches {
        let same: Vec<&str> = branches
            .iter()
            .filter(|b| b.repository.id.eq_ignore_ascii_case(&branch.repository.id))
            .map(|b| b.branch.as_str())
            .collect();
        if same.len() > 1 {
            return Err(invalid(format!(
                "Choose one branch of repository {}: {}.",
                branch.repository.name,
                same.join(", ")
            )));
        }
    }
    let listed: Vec<String> = branches
        .iter()
        .map(|b| format!("branch {} in {}", b.branch, b.repository.name))
        .chain(pages.iter().map(|p| format!("Wiki page {}", p.path)))
        .collect();
    if !args.confirmed {
        return Err(invalid(format!(
            "Confirm with the user, then accept with confirmed: true. Accept as the delivery of {}: {}. The run then goes on by itself: it waits for the pull requests of those branches (a Wiki page needs none); nothing is written to Azure DevOps.",
            child.url,
            listed.join("; ")
        )));
    }
    let child = &mut run.children[index];
    for branch in &branches {
        match &child.repository {
            Some(own)
                if own.id.eq_ignore_ascii_case(&branch.repository.id) && child.branch.is_none() =>
            {
                child.branch = Some(branch.branch.clone());
            }
            None => {
                child.repository = Some(branch.repository.clone());
                child.branch = Some(branch.branch.clone());
            }
            _ => child.branches.push(RepositoryBranch {
                repository: branch.repository.clone(),
                branch: branch.branch.clone(),
            }),
        }
    }
    child.wiki_pages = pages;
    child.outcome = Some("done".into());
    child.finished_at = child.finished_at.or(Some(at));
    child.found = Some(fresh);
    let next = if run.children.iter().any(|c| c.outcome.is_none()) {
        Phase::Coding
    } else {
        run.waiting_since = Some(at);
        run.checked_at = None;
        Phase::PullRequests
    };
    run.advance(next, at, "accepted what was found");
    run.error = None;
    state.workflows.db.save(&run).map_err(fail)?;
    Ok(json!({"run": run.view(), "accepted": listed}))
}

/// What `workflow_run_accept_found` with `args` would take, from the run's status as recorded
/// (the person's confirmation names it; the host reads it again and takes only what is still
/// there), or none when the status names nothing to accept.
pub(in crate::app) fn describe_accept(status: &Value, args: &Value) -> Option<String> {
    let children = status["run"]["children"].as_array()?;
    let child = children.iter().find(|c| match args["child"].as_u64() {
        Some(id) => c["id"].as_u64() == Some(id),
        None => c["found"].is_object() && c["branch"].is_null() && c["wiki_pages"].is_null(),
    })?;
    let found = &child["found"];
    let chosen = |list: &str| args[list].as_array().filter(|a| !a.is_empty());
    let everything = ["branches", "pull_requests", "wiki_pages"]
        .iter()
        .all(|l| chosen(l).is_none());
    let mut parts: Vec<String> = Vec::new();
    for branch in found["branches"].as_array().into_iter().flatten() {
        let (name, repository) = (branch["branch"].as_str()?, &branch["repository"]);
        let picked = chosen("branches").is_some_and(|c| {
            c.iter().any(|b| {
                b["branch"].as_str() == Some(name)
                    && [&repository["name"], &repository["id"]].iter().any(|r| {
                        r.as_str()
                            .zip(b["repository"].as_str())
                            .is_some_and(|(r, b)| r.eq_ignore_ascii_case(b))
                    })
            })
        }) || chosen("pull_requests").is_some_and(|c| {
            found["pull_requests"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|p| p["source"].as_str() == Some(name) && c.contains(&p["id"]))
        });
        if everything || picked {
            parts.push(format!("branch {name} in {}", repository["name"].as_str()?));
        }
    }
    for page in found["wiki_pages"].as_array().into_iter().flatten() {
        let path = page["path"].as_str()?;
        if everything
            || chosen("wiki_pages").is_some_and(|c| {
                c.iter()
                    .any(|w| w.as_str() == Some(path) || *w == page["url"])
            })
        {
            parts.push(format!("Wiki page {path}"));
        }
    }
    (!parts.is_empty()).then(|| {
        format!(
            "Accept as the delivery of {} in workflow run {}: {} (only what is still found when it is read again), so the run goes on by itself? Nothing is written to Azure DevOps.",
            child["url"].as_str().unwrap_or("its child"),
            args["run"].as_str().unwrap_or("this"),
            parts.join("; ")
        )
    })
}

fn same_branch(a: &FoundBranch, b: &FoundBranch) -> bool {
    a.repository.id.eq_ignore_ascii_case(&b.repository.id) && a.branch == b.branch
}
