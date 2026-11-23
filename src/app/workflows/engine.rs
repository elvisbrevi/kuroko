//! The workflow run engine: a tracked task of the coordinator's control loop that, every 30
//! seconds, advances the workflow runs (`record`) through the existing implementation
//! operations and observes their records. It never starts a provider process itself (it asks
//! for implementation runs with `implementation_launch`, the call `implementation_start`
//! makes), never creates children (the user does, with `implementation_create`; the engine
//! reads the creation ledger), and never writes to Azure DevOps or merges, completes or changes
//! a pull request: it reads them.
//!
//! Each step is durable before it acts: the ID of an implementation run is recorded before the
//! machine is asked to launch it, and the launch is idempotent by that ID, so an interruption
//! anywhere resumes from the last recorded stage without a second start. Only the coordinator
//! advances runs (never a computer signed in to one, never a shared portal account that is not
//! the owner's), at most `concurrency` of them in the stages that take a slot. Slots go first in,
//! first out: a run takes one only when no run created before it waits for one (`record::line`),
//! whichever run's task asks first, so a run that gave a slot back keeps its place. Classifying
//! takes no slot: the director's sessions run beside the coding runs, one at a time.
//!
//! While a pause (`record::Pause`) covers a run's project, the engine starts nothing for it (no
//! slot, director session, planning or coding run or acting block) and still observes what
//! already runs, so a session left to finish ends in the record as it would otherwise.
use super::acting;
use super::record::{self, Child, Implementation, Item, Run, State as Phase, Store};
use super::*;
use crate::ado::workflow::PullRequest;
use crate::agent_prompts::Texts;
use crate::tools::todo::Detail;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// How often the engine looks at the runs.
pub(in crate::app) const TICK_SECONDS: u64 = 30;
/// How often one run's pull requests are read while it waits for them.
pub(in crate::app) const PULL_REQUEST_POLL_SECONDS: i64 = 300;
/// At most this many steps of one run in one tick.
const STEPS: usize = 16;
/// The characters a run's context takes (`crate::workflows::MAX_CONTEXT`).
const CONTEXT: usize = crate::workflows::MAX_CONTEXT;

/// The engine's state on the profile: the runs' store and what advances now.
pub(in crate::app) struct State {
    pub(in crate::app) db: Store,
    /// The ready-state watcher: its seen ledger and last poll (`watcher`).
    pub(in crate::app) watcher: super::watcher::Watcher,
    /// The workflow supervisor: its events and looks, and whether one runs now.
    pub(in crate::app) supervision: super::supervisor::Supervision,
    /// What the runs' coding sessions found (`findings`).
    pub(in crate::app) findings: super::findings::Findings,
    /// The workflow leads' shifts (`shifts`).
    pub(in crate::app) shifts: super::shifts::Shifts,
    locks: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
    admission: Mutex<()>,
    /// One director classification at a time, apart from the `concurrency` slots.
    classifying: Mutex<()>,
    advancing: AtomicUsize,
    recovered: AtomicBool,
    /// The attempts left without a result were settled once after a start.
    settled: AtomicBool,
    /// The runs whose advance is running now, each in its own task.
    inflight: std::sync::Mutex<std::collections::HashSet<String>>,
}
impl State {
    pub(in crate::app) fn new(profile: &Path) -> Result<Self> {
        let root = profile.join("implementation");
        security::private_dir(&root)?;
        Ok(Self {
            db: Store::open(&root.join("workflows.db"))?,
            watcher: super::watcher::Watcher::open(&root.join("workflows.db"))?,
            supervision: super::supervisor::Supervision::open(&root.join("workflows.db"))?,
            findings: super::findings::Findings::open(&root.join("workflows.db"))?,
            shifts: super::shifts::Shifts::open(&root.join("workflows.db"))?,
            locks: std::sync::Mutex::new(HashMap::new()),
            admission: Mutex::new(()),
            classifying: Mutex::new(()),
            advancing: AtomicUsize::new(0),
            recovered: AtomicBool::new(false),
            settled: AtomicBool::new(false),
            inflight: std::sync::Mutex::new(std::collections::HashSet::new()),
        })
    }
    /// Held while something the updater waits for runs (a lead's turn).
    pub(in crate::app) fn busy(&self) -> Advancing<'_> {
        Advancing::new(&self.advancing)
    }
    /// The lock every change of one run holds: a step of the engine, a cancel, a resume.
    pub(in crate::app) fn lock(&self, id: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(id.to_owned())
            .or_default()
            .clone()
    }
    /// Whether a run is advancing a stage now (the updater waits).
    pub(in crate::app) fn advancing(&self) -> bool {
        self.advancing.load(Ordering::SeqCst) > 0
    }
    /// Whether the look after a start found every active implementation run.
    pub(in crate::app) fn recovered(&self) -> bool {
        self.recovered.load(Ordering::SeqCst)
    }
}
/// A run's advance in flight; dropped when its task ends.
struct Inflight(Arc<Host>, String);
impl Drop for Inflight {
    fn drop(&mut self) {
        self.0
            .state
            .workflows
            .inflight
            .lock()
            .unwrap()
            .remove(&self.1);
    }
}
pub(in crate::app) struct Advancing<'a>(&'a AtomicUsize);
impl<'a> Advancing<'a> {
    fn new(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}
impl Drop for Advancing<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Where the engine reads Azure DevOps: the enabled source with its read credential, or a
/// test's synthetic catalog. Neither has a way to write.
#[derive(Clone)]
pub(in crate::app) enum Azure {
    Live,
    #[cfg(test)]
    #[cfg_attr(not(unix), allow(dead_code))]
    Synthetic(Arc<super::synthetic::Catalog>),
}
impl Azure {
    /// An item of any category, read fresh.
    pub(in crate::app) async fn item(
        &self,
        state: &DesktopState,
        source: Option<&str>,
        project: Option<&str>,
        id: u64,
    ) -> Result<Detail, String> {
        match self {
            Self::Live => {
                let body = json!({"id":id,"project":project,"source":source});
                let value = tools::todo::read(state, "get_azure_item", &body).await?;
                serde_json::from_value(value).map_err(fail)
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => catalog.item(id),
        }
    }
    pub(in crate::app) fn assigned(
        &self,
        state: &DesktopState,
        source: Option<&str>,
        detail: &Detail,
    ) -> Result<bool, String> {
        match self {
            Self::Live => tools::todo::assigned(state, source, detail),
            #[cfg(test)]
            Self::Synthetic(catalog) => Ok(catalog.assigned(detail.item.id)),
        }
    }
    async fn pull_requests(
        &self,
        state: &DesktopState,
        run: &Run,
        repository: &crate::workflows::Repository,
        branch: &str,
    ) -> Result<Vec<PullRequest>, String> {
        match self {
            Self::Live => {
                tools::todo::pull_requests(
                    state,
                    run.item.source.as_deref(),
                    &run.item.organization,
                    &run.item.project,
                    (&repository.id, &repository.name),
                    branch,
                )
                .await
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => catalog.pull_requests(&repository.id, branch),
        }
    }
    /// The pull requests of a child's reported branch, read now (the workflow supervisor).
    pub(in crate::app) async fn branch_pulls(
        &self,
        state: &DesktopState,
        run: &Run,
        repository: &crate::workflows::Repository,
        branch: &str,
    ) -> Result<Vec<PullRequest>, String> {
        self.pull_requests(state, run, repository, branch).await
    }
    /// The bounded summaries of these implementation runs' own sessions (the workflow
    /// supervisor), and whether some could not be read: through the machine channel, or a
    /// test's synthetic summaries, never this computer's own agent stores.
    pub(in crate::app) async fn run_sessions(
        &self,
        host: &Arc<Host>,
        runs: Vec<String>,
    ) -> Result<(Vec<Value>, bool)> {
        match self {
            Self::Live => implementation::run_session_summaries(host, runs).await,
            #[cfg(test)]
            Self::Synthetic(catalog) => Ok((catalog.sessions(&runs), false)),
        }
    }
    /// The predecessor graph of a work item of the run's organization, read now.
    pub(in crate::app) async fn dependencies(
        &self,
        state: &DesktopState,
        run: &Run,
        id: u64,
    ) -> Result<crate::ado::dependencies::Graph, String> {
        match self {
            Self::Live => {
                tools::todo::dependencies(
                    state,
                    run.item.source.as_deref(),
                    &run.item.organization,
                    id,
                )
                .await
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => catalog.graph(id),
        }
    }
    /// The default branch of each repository of the run's project, by lowercase ID (cached for
    /// five minutes like the catalog `workflow_repositories` reads).
    pub(in crate::app) async fn default_branches(
        &self,
        state: &DesktopState,
        run: &Run,
    ) -> Result<HashMap<String, String>, String> {
        match self {
            Self::Live => {
                let catalog = tools::todo::repositories(
                    state,
                    run.item.source.as_deref(),
                    Some(&run.item.organization),
                    &run.item.project,
                )
                .await?;
                Ok(catalog["repositories"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| {
                        let branch = r["default_branch"].as_str().filter(|b| !b.is_empty())?;
                        Some((r["id"].as_str()?.to_lowercase(), branch.to_owned()))
                    })
                    .collect())
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => Ok(catalog.default_branches()),
        }
    }
    /// A workflow lead's read of its workflow's project: a Wiki page, a file or a folder.
    pub(in crate::app) async fn lead_read(
        &self,
        state: &DesktopState,
        workflow: &crate::workflows::Workflow,
        read: tools::todo::LeadRead<'_>,
    ) -> Result<Value, String> {
        match self {
            Self::Live => {
                let source = tools::todo::source_for(state, &workflow.organization)?;
                tools::todo::lead_read(
                    state,
                    source.as_deref(),
                    (&workflow.organization, &workflow.project),
                    read,
                )
                .await
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => catalog.lead_read(read),
        }
    }
    /// The comment threads of a run's pull request, read now (review rounds).
    pub(in crate::app) async fn threads(
        &self,
        state: &DesktopState,
        run: &Run,
        repository: &str,
        pull_request: u64,
    ) -> Result<Vec<crate::ado::workflow::Thread>, String> {
        match self {
            Self::Live => {
                tools::todo::threads(
                    state,
                    run.item.source.as_deref(),
                    &run.item.organization,
                    &run.item.project,
                    repository,
                    pull_request,
                )
                .await
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => Ok(catalog.threads(pull_request)),
        }
    }
    /// The files a run's pull request conflicts on with its target branch, read now.
    pub(in crate::app) async fn conflicts(
        &self,
        state: &DesktopState,
        run: &Run,
        repository: &str,
        pull_request: u64,
    ) -> Result<Vec<String>, String> {
        match self {
            Self::Live => {
                tools::todo::conflicts(
                    state,
                    run.item.source.as_deref(),
                    &run.item.organization,
                    &run.item.project,
                    repository,
                    pull_request,
                )
                .await
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => Ok(catalog.conflicts(pull_request)),
        }
    }
    /// The branches the user pushed to a workflow repository since a time: what a coding run
    /// may have delivered without reporting it.
    pub(in crate::app) async fn pushed_branches(
        &self,
        state: &DesktopState,
        run: &Run,
        repository: &crate::workflows::Repository,
        window: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>),
    ) -> Result<(Vec<crate::ado::workflow::Pushed>, bool), String> {
        match self {
            Self::Live => {
                tools::todo::pushed_branches(
                    state,
                    run.item.source.as_deref(),
                    &run.item.organization,
                    &run.item.project,
                    (&repository.id, &repository.name),
                    window,
                )
                .await
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => {
                Ok((catalog.pushed_branches(&repository.id, window), false))
            }
        }
    }
    /// One pull request of a workflow repository by its ID, when the user opened it since a
    /// time (one linked to a work item).
    pub(in crate::app) async fn pull_request(
        &self,
        state: &DesktopState,
        run: &Run,
        repository: &crate::workflows::Repository,
        number: u64,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<PullRequest>, String> {
        match self {
            Self::Live => {
                tools::todo::pull_request(
                    state,
                    run.item.source.as_deref(),
                    &run.item.organization,
                    &run.item.project,
                    (&repository.id, &repository.name),
                    number,
                    since,
                )
                .await
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => Ok(catalog.pull_request(&repository.id, number, since)),
        }
    }
    /// The pages of the run's project Wiki the user changed since a time, and whether part of
    /// it could not be read.
    pub(in crate::app) async fn wiki_pages(
        &self,
        state: &DesktopState,
        run: &Run,
        window: (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>),
    ) -> Result<(Vec<crate::ado::workflow::WikiPage>, bool), String> {
        match self {
            Self::Live => {
                tools::todo::wiki_pages(
                    state,
                    run.item.source.as_deref(),
                    &run.item.organization,
                    &run.item.project,
                    window,
                )
                .await
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => Ok((catalog.wiki_pages(window), false)),
        }
    }
    /// Whether a branch exists in a workflow repository now (the workflow supervisor).
    pub(in crate::app) async fn branch_exists(
        &self,
        state: &DesktopState,
        run: &Run,
        repository: &str,
        branch: &str,
    ) -> Result<bool, String> {
        match self {
            Self::Live => {
                tools::todo::branch_exists(
                    state,
                    run.item.source.as_deref(),
                    &run.item.organization,
                    &run.item.project,
                    repository,
                    branch,
                )
                .await
            }
            #[cfg(test)]
            Self::Synthetic(catalog) => Ok(catalog.branch_exists(repository, branch)),
        }
    }
}

/// Whether this installation advances workflow runs: the account's coordinator, with the
/// owner's authority.
pub(in crate::app) fn may_run(state: &DesktopState) -> bool {
    implementation::link(state).is_ok_and(|l| l.is_none())
        && authority::invocation(state).is_ok_and(|i| i.scope.owner)
}

/// The workflow a run is assigned now: its definition and blocks as they are, and the stage it
/// begins with (planning, or coding for a task-category item, which is its own child).
pub(in crate::app) fn assign(
    run: &mut Run,
    workflow: &crate::workflows::Workflow,
    blocks: &[crate::workflows::Block],
    now: i64,
    outcome: &str,
) {
    run.workflow = Some(workflow.id.clone());
    run.definition = Some(workflow.clone());
    run.blocks = workflow
        .blocks
        .iter()
        .filter_map(|id| blocks.iter().find(|b| &b.id == id).cloned())
        .collect();
    if run.task() {
        run.children = vec![Child {
            id: run.item.id,
            title: run.item.title.clone(),
            url: run.item.url.clone(),
            kind: run.item.kind.clone(),
            ..Default::default()
        }];
        run.advance(Phase::Coding, now, outcome);
    } else {
        run.advance(Phase::Planning, now, outcome);
    }
}

/// One look at every run: recover once after a start, then advance each run the engine drives.
pub(in crate::app) async fn tick(
    host: &Arc<Host>,
    azure: &Azure,
    now: i64,
    poll: i64,
) -> Result<()> {
    let state = &host.state.workflows;
    if !may_run(&host.state) {
        return Ok(());
    }
    // Until every active implementation run has been looked at once, the runs whose run could
    // not be asked wait: the normal step would read an interruption as an ending.
    let unrecovered = if state.recovered() {
        std::collections::HashSet::new()
    } else {
        let unrecovered = recover(host).await?;
        if unrecovered.is_empty() {
            state.recovered.store(true, Ordering::SeqCst);
        }
        unrecovered
    };
    // After a start, every acting attempt left without a result is settled, in every run.
    if !state.settled.load(Ordering::SeqCst) {
        acting::settle_all(host, now).await?;
        state.settled.store(true, Ordering::SeqCst);
    }
    // A failed run is not advanced, but its `failed` blocks may be due (the host stopped before
    // they ran, or a launcher was busy): they stay in the sweep until each has an outcome.
    let mut tasks = Vec::new();
    let pauses = state.db.pauses()?;
    for run in state.db.list(&[Phase::Failed], None, -1)? {
        if acting::has_due(&run)
            && !record::paused(&pauses, &run.item.project)
            && state.inflight.lock().unwrap().insert(run.id.clone())
        {
            let guard = Inflight(host.clone(), run.id.clone());
            let (host, id) = (host.clone(), run.id.clone());
            tasks.push(tokio::spawn(async move {
                let _guard = guard;
                let _ = acting::execute(&host, &id, now).await;
            }));
        }
    }
    // A run that waits for the person after a coding session stopped while it worked: the
    // session they continue to its final report is delivered by itself (`watch_continued`).
    for run in state.db.list(&[Phase::NeedsAttention], None, -1)? {
        if unrecovered.contains(&run.id)
            || !continued_due(&run, now, poll)
            || !state.inflight.lock().unwrap().insert(run.id.clone())
        {
            continue;
        }
        let guard = Inflight(host.clone(), run.id.clone());
        let (host, azure, id) = (host.clone(), azure.clone(), run.id.clone());
        tasks.push(tokio::spawn(async move {
            let _guard = guard;
            let state = &host.state.workflows;
            let lock = state.lock(&id);
            let _held = lock.lock().await;
            let Ok(Some(mut run)) = state.db.get(&id) else {
                return;
            };
            if run.state != Phase::NeedsAttention {
                return;
            }
            let advanced = run.state;
            if watch_continued(&host, &azure, &mut run, now, poll).await {
                if run.state != advanced {
                    run.updated_at = now;
                }
                let _ = state.db.save(&run);
            }
        }));
    }
    let driven: Vec<Phase> = Phase::ALL.into_iter().filter(|s| s.driven()).collect();
    let mut runs = state.db.list(&driven, None, -1)?;
    // The oldest first; which of them takes a free slot is decided by the line (`admit`), not
    // by the order their tasks ask in.
    runs.reverse();
    for run in runs {
        if unrecovered.contains(&run.id) || !state.inflight.lock().unwrap().insert(run.id.clone()) {
            continue;
        }
        // Each run in its own task: a slow step (a director session) never holds the others.
        let guard = Inflight(host.clone(), run.id.clone());
        let (host, azure, id) = (host.clone(), azure.clone(), run.id.clone());
        tasks.push(tokio::spawn(async move {
            let _guard = guard;
            if advance(&host, &azure, &id, now, poll).await.is_err() {
                tracing::warn!(event = "workflow_run_not_advanced");
            }
        }));
    }
    // A tick waits for its steps at most until the next one; a run still advancing then is
    // skipped by the next tick and keeps going on its own.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(TICK_SECONDS),
        futures_util::future::join_all(tasks),
    )
    .await;
    // The leads see what changed; a turn that is due runs in its own task.
    super::shifts::tick(host, azure, now).await;
    Ok(())
}

/// After a restart: a run whose active implementation run was interrupted is `interrupted`.
/// Returns the runs whose implementation run could not be asked (its machine is unreachable).
async fn recover(host: &Arc<Host>) -> Result<std::collections::HashSet<String>> {
    let state = &host.state.workflows;
    let mut unrecovered = std::collections::HashSet::new();
    for run in state.db.list(
        &[Phase::Planning, Phase::Coding, Phase::AddressingReview],
        None,
        -1,
    )? {
        let lock = state.lock(&run.id);
        let _guard = lock.lock().await;
        let Some(mut run) = state.db.get(&run.id)? else {
            continue;
        };
        let Some(active) = run.active_implementation().cloned() else {
            continue;
        };
        let observed = match implementation::observe(host, &active.machine, &active.id).await {
            Ok(Some(observed)) => observed,
            Ok(None) => continue,
            Err(_) => {
                unrecovered.insert(run.id.clone());
                continue;
            }
        };
        if observed["state"] == "interrupted" {
            set_observed(&mut run, "interrupted");
            // Why it stopped, as every other ending records it: the host restarted.
            set_failure(
                &mut run,
                observed_failure(&observed).unwrap_or(crate::llm::implementation::Failure::of(
                    crate::llm::implementation::Cause::HostRestarted,
                )),
            );
            let now = chrono::Utc::now().timestamp();
            run.stop(
                Phase::Interrupted,
                now,
                format!(
                    "The host restarted while implementation run {} was active, so it was interrupted; resume to continue from this stage (a started coding run is not started again).",
                    active.id
                ),
            );
            state.db.save(&run)?;
            stop_sessions(host, &run, None).await;
        }
    }
    Ok(unrecovered)
}

/// Stop every agent session of the run's implementation runs that is still alive, a session
/// kept after its run included (`implementation_cancel` on its machine), except `except`'s.
/// Returns the implementation runs whose session was stopped and, when a machine could not be
/// reached, what to do about each run it holds.
pub(in crate::app) async fn stop_sessions(
    host: &Arc<Host>,
    run: &Run,
    except: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let (mut stopped, mut warnings) = (Vec::new(), Vec::new());
    let mut unreachable = std::collections::HashSet::new();
    for implementation in run.implementations() {
        if except == Some(implementation.id.as_str()) {
            continue;
        }
        let alive = implementation::session_life(host, &implementation.id)
            .await
            .is_some_and(|life| life["session_alive"] == true);
        if !alive {
            continue;
        }
        let machine = &implementation.machine;
        let reached = !unreachable.contains(machine)
            && implementation::cancel_run(host, machine, &implementation.id)
                .await
                .is_ok();
        if reached {
            stopped.push(implementation.id.clone());
        } else {
            unreachable.insert(machine.clone());
            warnings.push(format!(
                "The session of implementation run {} may still be alive on machine {}, which could not be reached; once it is reachable, stop it there with implementation_cancel (machine {}, id {}).",
                implementation.id, machine, machine, implementation.id
            ));
        }
    }
    (stopped, warnings)
}

/// What ending a run's sessions did: the active implementation run cancelled on its machine,
/// the other sessions stopped, and what could not be reached.
pub(in crate::app) struct Ended {
    pub cancelled: Option<String>,
    pub stopped: Vec<String>,
    pub warning: Option<String>,
}
/// Cancel the run's active implementation run when it is still in flight, and stop every other
/// session it started that is still alive (`stop_sessions`); the file changes stay. When its
/// machine cannot be asked, the run may be running there (even one whose launch was not
/// confirmed yet): the cancellation is tried, and when it does not reach the machine the warning
/// says where to cancel it.
pub(in crate::app) async fn end_sessions(host: &Arc<Host>, run: &Run) -> Ended {
    let active = run.active_implementation().cloned();
    let (mut cancelled, mut warning, mut asked) = (None, None, None);
    if let Some(active) = &active {
        let present = match implementation::observe(host, &active.machine, &active.id).await {
            Ok(Some(observed)) => {
                matches!(observed["state"].as_str(), Some("running" | "planning"))
            }
            Ok(None) => false,
            Err(_) => true,
        };
        if present {
            asked = Some(active.id.as_str());
            match implementation::cancel_run(host, &active.machine, &active.id).await {
                Ok(()) => cancelled = Some(active.id.clone()),
                Err(_) => {
                    warning = Some(format!(
                        "Implementation run {} may still be running on machine {}, which could not be reached; once it is reachable, cancel it there with implementation_cancel (machine {}, id {}).",
                        active.id, active.machine, active.machine, active.id
                    ))
                }
            }
        }
    }
    // Every other session the run started and that is still alive stops too: a coding session
    // kept after its run, or one the user opened again.
    let (stopped, unreached) = stop_sessions(host, run, asked).await;
    let warning = warning
        .into_iter()
        .chain(unreached)
        .reduce(|all, next| format!("{all} {next}"));
    Ended {
        cancelled,
        stopped,
        warning,
    }
}

/// Record the observed state of the run's active implementation run.
/// Why an implementation run another machine returned did not end with its answer, as it
/// reported it (`Failure::of_record`).
pub(super) fn observed_failure(observed: &Value) -> Option<crate::llm::implementation::Failure> {
    crate::llm::implementation::Failure::of_record(observed)
}
/// The time limit of a `workflow` session, in minutes: its agent's own, else the workflow
/// settings' for its kind (planning, or coding and review rounds).
pub(super) fn time_limit(
    host: &Arc<Host>,
    workflow: &crate::workflows::Workflow,
    planning: bool,
) -> u32 {
    host.state
        .settings
        .map()
        .map(|map| {
            map.workflow_settings
                .time_limit_minutes(Some(workflow), planning)
        })
        .unwrap_or_else(|_| {
            workflow
                .agent
                .time_limit_minutes
                .unwrap_or(crate::workflows::director::DEFAULT_TIME_LIMIT_MINUTES)
        })
}
/// The time limit a launched session's record keeps: none for the default two hours, so the
/// record stays readable by a host from before the limit was configurable
/// (`deny_unknown_fields`); a status reads none as the default.
pub(super) fn recorded_limit(minutes: u32) -> Option<u32> {
    (minutes != crate::workflows::director::DEFAULT_TIME_LIMIT_MINUTES).then_some(minutes)
}
/// `ended {state}`, with the cause's sentence when there is one.
pub(super) fn ended(state: &str, failure: Option<crate::llm::implementation::Failure>) -> String {
    match failure {
        Some(failure) => format!("ended {state}: {}", failure.sentence()),
        None => format!("ended {state}."),
    }
}

fn set_observed(run: &mut Run, observed: &str) {
    let stage = run.stopped_in.unwrap_or(run.state);
    let target = match stage {
        Phase::Planning => run.planning.as_mut(),
        Phase::Coding => run
            .children
            .iter_mut()
            .find(|c| c.outcome.is_none())
            .and_then(|c| c.coding.as_mut()),
        Phase::AddressingReview => run
            .children
            .iter_mut()
            .find_map(|c| c.rounds.last_mut().filter(|r| r.outcome.is_none()))
            .and_then(|r| r.coding.as_mut()),
        _ => None,
    };
    if let Some(target) = target {
        target.state = Some(observed.to_owned());
    }
}

/// Record why the run's active implementation run did not end with its answer.
fn set_failure(run: &mut Run, failure: crate::llm::implementation::Failure) {
    let stage = run.stopped_in.unwrap_or(run.state);
    let target = match stage {
        Phase::Planning => run.planning.as_mut(),
        Phase::Coding => run
            .children
            .iter_mut()
            .find(|c| c.outcome.is_none())
            .and_then(|c| c.coding.as_mut()),
        Phase::AddressingReview => run
            .children
            .iter_mut()
            .find_map(|c| c.rounds.last_mut().filter(|r| r.outcome.is_none()))
            .and_then(|r| r.coding.as_mut()),
        _ => None,
    };
    if let Some(target) = target {
        target.failure = Some(failure);
    }
}

/// Take a slot for a run in a stage that needs one, when one is free and no run created before
/// it waits for one.
async fn admit(host: &Arc<Host>, run: &mut Run, now: i64) -> Result<bool> {
    let state = &host.state.workflows;
    let _admission = state.admission.lock().await;
    let concurrency = host.state.settings.map()?.workflow_settings.concurrency as usize;
    let driven: Vec<Phase> = Phase::ALL.into_iter().filter(|s| s.driven()).collect();
    let mut runs = state.db.list(&driven, None, -1)?;
    runs.reverse();
    let pauses = state.db.pauses()?;
    if !record::may_take_slot(&runs, &run.id, concurrency, now, &pauses) {
        return Ok(false);
    }
    run.admitted = true;
    // Moved ahead for one admission only.
    run.ahead_since = None;
    state.db.save(run)?;
    Ok(true)
}

/// Advance one run as far as it goes now.
async fn advance(host: &Arc<Host>, azure: &Azure, id: &str, now: i64, poll: i64) -> Result<()> {
    let state = &host.state.workflows;
    let lock = state.lock(id);
    for _ in 0..STEPS {
        let guard = lock.lock().await;
        let Some(mut run) = state.db.get(id)? else {
            return Ok(());
        };
        if !run.state.driven() {
            return Ok(());
        }
        if run.retry_at.is_some_and(|at| now < at) {
            return Ok(());
        }
        // Paused: nothing starts, not even a slot taken; what runs is still observed.
        let paused = record::paused(&state.db.pauses()?, &run.item.project);
        if paused && (run.waits_for_slot() || run.state == Phase::Classifying) {
            return Ok(());
        }
        // Its next launch needs checkouts another run's agent holds on that machine: it waits
        // for them without a slot (giving back one it took), instead of being refused.
        if run.state.takes_slot() && record::needs(&run).is_some() {
            let driven: Vec<Phase> = Phase::ALL.into_iter().filter(|s| s.driven()).collect();
            let others = state.db.list(&driven, None, -1)?;
            if let Some(holder) = record::held_by(&others, &run) {
                let why = held(holder);
                let released = std::mem::take(&mut run.admitted);
                if wait(&mut run, why).unwrap_or(false) || released {
                    run.updated_at = now;
                    state.db.save(&run)?;
                }
                return Ok(());
            }
        }
        if run.waits_for_slot() && !admit(host, &mut run, now).await? {
            return Ok(());
        }
        let _advancing = Advancing::new(&state.advancing);
        if run.state == Phase::Classifying {
            // The director's session can take a minute or two: the run's lock is not held
            // meanwhile, so a cancel is not kept waiting; its verdict applies only to a run
            // still classifying.
            drop(guard);
            if !classify(host, azure, run, now).await? {
                return Ok(());
            }
            continue;
        }
        // The acting blocks due at this stage run before the stage's own step; the run's lock
        // is not held while they act. Paused, they wait for the pause to end (each occurrence
        // stays due, and the stage of `blocks` does not end meanwhile).
        if acting::has_due(&run) && !paused {
            drop(guard);
            match acting::execute(host, id, now).await? {
                acting::Progress::Waiting => return Ok(()),
                acting::Progress::Idle | acting::Progress::Changed => continue,
            }
        }
        match step(host, azure, &mut run, now, poll, paused).await {
            Ok(true) => {
                run.updated_at = now;
                state.db.save(&run)?;
                if run.state.stops_sessions() {
                    drop(guard);
                    stop_sessions(host, &run, None).await;
                    return Ok(());
                }
            }
            Ok(false) => return Ok(()),
            Err(error) => {
                let redactor =
                    security::Redactor::new(&[], Vec::new()).expect("built-in redaction patterns");
                run.stop(Phase::Failed, now, redactor.redact(&error));
                state.db.save(&run)?;
                drop(guard);
                stop_sessions(host, &run, None).await;
                // The blocks of the `failed` stage (once the pause ends, when paused).
                if !paused {
                    acting::execute(host, id, now).await?;
                }
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Note why a run waits (a machine offline, a read that failed); a change only when it is new.
pub(super) fn wait(run: &mut Run, why: String) -> Result<bool, String> {
    if run.error.as_deref() == Some(why.as_str()) {
        return Ok(false);
    }
    run.error = Some(why);
    Ok(true)
}

/// Why a run waits for checkouts: the run whose agent holds them, and where.
pub(in crate::app) fn held(holder: &Run) -> String {
    let machine = record::holds(holder).map(|(m, _)| m).unwrap_or_default();
    format!(
        "Waiting for the checkouts that workflow run {} ({}) is using on machine {machine}; it continues when they are free.",
        holder.id, holder.item.url
    )
}

/// A launch refusal the run waits through (the machine is offline or busy with these
/// directories) rather than one that fails it.
pub(super) fn transient(error: &str) -> bool {
    error.contains("offline")
        || error.contains(implementation::DIRECTORIES_BUSY)
        || error.contains("could not be reached")
        || error.contains("did not answer")
}

/// Ask the director and apply its verdict, when the run is still classifying; returns whether
/// the run changed.
async fn classify(host: &Arc<Host>, azure: &Azure, run: Run, now: i64) -> Result<bool> {
    let state = &host.state.workflows;
    let classifying = state.classifying.lock().await;
    let map = host.state.settings.map()?;
    // The project's director: its own, its workflows' shared agent or the account's.
    let directed = super::director::director_for(&map, &run.item.organization, &run.item.project);
    let verdict: std::result::Result<Value, String> = match directed {
        None => Err("No workflow director is configured; assign a workflow to this run.".into()),
        Some(_) => {
            match azure
                .item(
                    &host.state,
                    run.item.source.as_deref(),
                    Some(&run.item.project),
                    run.item.id,
                )
                .await
            {
                Ok(detail) => {
                    Box::pin(super::director::classify_item(
                        host,
                        &detail,
                        run.auto_start,
                    ))
                    .await
                }
                Err(error) => Err(error),
            }
        }
    };
    drop(classifying);
    let lock = state.lock(&run.id);
    let _guard = lock.lock().await;
    let Some(mut run) = state.db.get(&run.id)? else {
        return Ok(false);
    };
    if run.state != Phase::Classifying {
        return Ok(false);
    }
    let verdict = match verdict {
        // The director's machine is offline or the item could not be read: try again later.
        Err(error) if error.starts_with("[not_ready]") || error.starts_with("[network]") => {
            let note = error.split_once("] ").map_or(error.as_str(), |(_, t)| t);
            run.error = Some(note.to_owned());
            // Asked again later, one minute doubling up to half an hour; meanwhile it is out of
            // the line, so an unreachable director never holds up other runs, and back in its
            // place once its retry is due.
            run.retries = run.retries.saturating_add(1);
            run.retry_at = Some(now + (60i64 << run.retries.min(5).saturating_sub(1)).min(1800));
            run.updated_at = now;
            state.db.save(&run)?;
            return Ok(false);
        }
        Err(error) => {
            let note = error.split_once("] ").map_or(error.as_str(), |(_, t)| t);
            run.classification = Some(json!({"workflow":null,"outcome":"none","reason":note}));
            run.advance(Phase::Unassigned, now, "no workflow");
            run.attention = Some(note.to_owned());
            state.db.save(&run)?;
            return Ok(true);
        }
        Ok(verdict) => verdict,
    };
    let mut classification = json!({
        "workflow": verdict["workflow"],
        "outcome": verdict["outcome"],
        "reason": verdict["reason"],
        "director": verdict["director"],
        "session": verdict["run"],
    });
    // The director's session gave no answer: why, from the fixed vocabulary.
    let failure = (!verdict["failure"].is_null())
        .then(|| crate::llm::implementation::Failure::read(&verdict["failure"]));
    if let Some(failure) = failure {
        classification["failure"] = failure.view();
    }
    run.classification = Some(classification);
    // Checked against the workflows as they are now: one removed or disabled during the
    // session is not assigned.
    let map = host.state.settings.map()?;
    let chosen = verdict["workflow"].as_str().and_then(|id| {
        let workflow = map.workflows.iter().find(|w| w.id == id && w.enabled)?;
        // A run the watcher started chooses only among the workflows that start by themselves.
        (same_project(workflow, &run.item) && (!run.auto_start || workflow.auto_start))
            .then_some(workflow)
    });
    match chosen {
        Some(workflow) => {
            run.error = None;
            assign(
                &mut run,
                workflow,
                &map.workflow_blocks,
                now,
                &format!("chosen: {}", workflow.id),
            )
        }
        None => {
            let reason = match verdict["workflow"].as_str() {
                Some(id) => {
                    format!("workflow {id} is no longer an enabled workflow of this project")
                }
                None => verdict["reason"].as_str().unwrap_or("none").to_owned(),
            };
            run.error = None;
            run.advance(Phase::Unassigned, now, "no workflow");
            run.attention = Some(match failure {
                Some(failure) => format!(
                    "The workflow director gave no answer: {} Assign a workflow to this run.",
                    failure.sentence()
                ),
                None => format!(
                    "The workflow director chose no workflow ({reason}); assign one to this run."
                ),
            });
        }
    }
    state.db.save(&run)?;
    Ok(true)
}

/// Whether a workflow covers the item's organization and project.
pub(in crate::app) fn same_project(workflow: &crate::workflows::Workflow, item: &Item) -> bool {
    crate::workflows::organization_name(&workflow.organization)
        == crate::workflows::organization_name(&item.organization)
        && workflow.project.eq_ignore_ascii_case(&item.project)
}

/// One step of a run in a driven stage other than classifying; returns whether it changed.
/// Paused, it launches no implementation run.
async fn step(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    now: i64,
    poll: i64,
    paused: bool,
) -> std::result::Result<bool, String> {
    let workflow = run
        .definition
        .clone()
        .ok_or("The run has no workflow definition.")?;
    match run.state {
        Phase::Planning => planning(host, azure, run, &workflow, now, paused).await,
        Phase::PlanReview => {
            let plan = run
                .planning
                .as_ref()
                .map(|p| p.id.clone())
                .ok_or("The run has no planning run.")?;
            let Some(record) = implementation::creation_record(host, &plan).await else {
                return Ok(false);
            };
            if record["parent"] != run.item.url.as_str() {
                run.stop(
                    Phase::NeedsAttention,
                    now,
                    format!(
                        "The creation ledger entry of planning run {plan} names another parent than {}; cancel this run, or resume it to plan again with a new planning run.",
                        run.item.url
                    ),
                );
                run.cause = Some("ledger_parent".into());
                return Ok(true);
            }
            run.ledger = Some(plan);
            run.advance(Phase::CreatingChildren, now, "confirmed");
            Ok(true)
        }
        Phase::CreatingChildren => creating(host, run, now).await,
        Phase::Coding => coding(host, azure, run, &workflow, now, paused).await,
        Phase::PullRequests => pull_requests(host, azure, run, now, poll).await,
        Phase::WaitingDependency => waiting(host, azure, run, now).await,
        Phase::AddressingReview => {
            super::review::step(host, azure, run, &workflow, now, paused).await
        }
        // Paused, its blocks have not run: the stage waits for the pause to end.
        Phase::Blocks if paused => Ok(false),
        Phase::Blocks => {
            // The blocks of this stage ran before this step (`acting`): nothing is due now.
            // Only the blocks of this stage's own occurrences count here.
            let own = |stage: Option<crate::workflows::Stage>| {
                matches!(
                    stage,
                    Some(
                        crate::workflows::Stage::PullRequestsCompleted
                            | crate::workflows::Stage::Done
                    )
                )
            };
            let due = run
                .blocks
                .iter()
                .any(|b| b.kind != crate::workflows::Kind::Instruction && own(b.stage));
            let outcome = if due {
                let count = run
                    .block_outcomes
                    .iter()
                    .filter(|o| o.occurrence == "pull_requests_completed" || o.occurrence == "done")
                    .count();
                format!("{count} acting block outcomes")
            } else {
                "skipped: no acting block".to_owned()
            };
            run.advance(Phase::Done, now, &outcome);
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The workflow's checkouts, in the order of its repositories.
fn checkouts(
    workflow: &crate::workflows::Workflow,
) -> Vec<(crate::workflows::Repository, PathBuf)> {
    workflow
        .repositories
        .iter()
        .filter_map(|r| {
            workflow
                .checkouts
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(&r.id))
                .map(|(_, d)| (r.clone(), d.clone()))
        })
        .collect()
}

/// The workflow's instructions and instruction blocks, for the agents: the user's
/// configuration, untrusted input under the fixed rules (`runs.workflow`, `runs.repository`,
/// `runs.instructions` and `runs.block`).
pub(in crate::app) fn instructions(
    workflow: &crate::workflows::Workflow,
    blocks: &[crate::workflows::Block],
    texts: &Texts,
) -> String {
    let mut text = texts.render(
        "runs.workflow",
        &[
            ("name", &workflow.name),
            ("id", &workflow.id),
            ("project", &workflow.project),
        ],
    );
    text.push('\n');
    for (repository, directory) in checkouts(workflow) {
        text.push_str(&texts.render(
            "runs.repository",
            &[
                ("repository", &repository.name),
                ("directory", &directory.display().to_string()),
            ],
        ));
        text.push('\n');
    }
    if !workflow.instructions.trim().is_empty() {
        text.push_str(&format!(
            "{}\n{}\n",
            texts.get("runs.instructions"),
            workflow.instructions
        ));
    }
    for block in blocks
        .iter()
        .filter(|b| b.kind == crate::workflows::Kind::Instruction)
    {
        text.push_str(&format!(
            "{}\n{}\n",
            texts.render("runs.block", &[("name", &block.name)]),
            block.text
        ));
    }
    text
}

/// The handover note as a coding context carries it, under its heading (`runs.handover_person`).
pub(in crate::app) fn handover(note: &str, texts: &Texts) -> String {
    format!(
        "{}\n",
        texts.render("runs.handover_person", &[("note", note)])
    )
}
/// A handover the workflow supervisor drafted and the person confirmed without editing: the
/// supervisor's text, untrusted like the evidence (`runs.handover_drafted`).
pub(in crate::app) fn drafted_handover(note: &str, texts: &Texts) -> String {
    format!(
        "{}\n",
        texts.render("runs.handover_drafted", &[("note", note)])
    )
}
/// A handover the workflow supervisor drafted and the workflow's pre-approval applied by itself:
/// nobody read it, so it is the supervisor's untrusted text and nothing more
/// (`runs.handover_preapproved`).
pub(in crate::app) fn preapproved_handover(note: &str, texts: &Texts) -> String {
    format!(
        "{}\n",
        texts.render("runs.handover_preapproved", &[("note", note)])
    )
}

/// The prompts in effect on this host: the owner's changes over the defaults.
pub(in crate::app) fn prompt_texts(host: &Host) -> Texts {
    host.state
        .settings
        .map()
        .map(|map| Texts::new(&map.agent_prompts))
        .unwrap_or_default()
}

/// A context as an implementation run takes it: whole, never cut. Saving a workflow refuses a
/// definition whose largest context does not fit (`largest_context`), so only a definition saved
/// by an older host can be refused here, with the numbers.
fn fitted(text: String) -> std::result::Result<String, String> {
    let used = text.chars().count();
    if used <= CONTEXT {
        return Ok(text);
    }
    Err(format!(
        "The run's context would take {used} characters, more than the {CONTEXT} a run takes, and it is never cut: shorten the workflow's instructions or instruction blocks (or the handover note) and restart the run, which takes the workflow as it is then."
    ))
}

/// The item a coding context names.
struct Subject<'a> {
    kind: &'a str,
    id: u64,
    title: &'a str,
    url: &'a str,
    /// The parent's link, for a planned child.
    parent: Option<&'a str>,
}

/// The sentence a coding context opens with: the item, the checkouts it is given (its own
/// repository's first, its working directory) and how to report its branches
/// (`runs.opening_one`, `runs.opening_many`).
fn opening(
    subject: &Subject,
    given: &[(crate::workflows::Repository, PathBuf)],
    texts: &Texts,
) -> String {
    let Some((repository, directory)) = given.first() else {
        return String::new();
    };
    let id = subject.id.to_string();
    let directory = directory.display().to_string();
    let mut values = vec![
        ("kind", subject.kind),
        ("id", id.as_str()),
        ("title", subject.title),
        ("url", subject.url),
        ("repository", repository.name.as_str()),
        ("directory", directory.as_str()),
    ];
    let text = if given.len() > 1 {
        let others: Vec<String> = given[1..]
            .iter()
            .map(|(r, d)| {
                texts.render(
                    "runs.other_checkout",
                    &[
                        ("repository", &r.name),
                        ("directory", &d.display().to_string()),
                    ],
                )
            })
            .collect();
        let others = others.join("; ");
        values.push(("others", &others));
        texts.render("runs.opening_many", &values)
    } else {
        let parent = subject
            .parent
            .map(|p| texts.render("runs.child_of", &[("parent", p)]))
            .unwrap_or_default();
        values.push(("parent", &parent));
        texts.render("runs.opening_one", &values)
    };
    format!("{text}\n")
}

/// The handover note a restarted run's coding runs get, and whose it is.
#[derive(Clone, Copy)]
enum Note<'a> {
    /// The person's note.
    Person(&'a str),
    /// The workflow supervisor's draft, which the person confirmed unedited.
    Drafted(&'a str),
    /// The workflow supervisor's draft, applied by the workflow's pre-approval: nobody read it.
    Preapproved(&'a str),
}

/// A coding run's context: the opening sentence, the handover note on the run this one
/// restarted with its preface (the person's, or the supervisor's draft), before the workflow's
/// text, then the workflow's instructions and instruction blocks.
fn coding_context(
    subject: &Subject,
    given: &[(crate::workflows::Repository, PathBuf)],
    note: Option<Note>,
    workflow: &crate::workflows::Workflow,
    blocks: &[crate::workflows::Block],
    texts: &Texts,
) -> String {
    let mut context = opening(subject, given, texts);
    // A run that may create work items declares their dependencies the way the engine reads them.
    if workflow.allow.contains(&crate::workflows::Allow::WorkItems) {
        context.push_str(&dependency_links(texts));
    }
    match note {
        Some(Note::Person(text)) => context.push_str(&handover(text, texts)),
        Some(Note::Drafted(text)) => context.push_str(&drafted_handover(text, texts)),
        Some(Note::Preapproved(text)) => context.push_str(&preapproved_handover(text, texts)),
        None => {}
    }
    context.push_str(&instructions(workflow, blocks, texts));
    context
}

/// What a child stacked on another's open pull request (`stack` policy) is told: where it
/// starts from and where its pull request goes (`runs.stacked`).
pub(in crate::app) fn stacked(base: &super::dependencies::Base, texts: &Texts) -> String {
    format!(
        "\n{}\n",
        texts.render(
            "runs.stacked",
            &[("branch", &base.branch), ("url", &base.url)]
        )
    )
}

/// How a coding run that may create work items declares that one needs another's code first
/// (`runs.dependency_links`).
pub(in crate::app) fn dependency_links(texts: &Texts) -> String {
    format!("\n{}\n", texts.get("runs.dependency_links"))
}

/// The longest item text a context is measured with: an item's kind, ID, title (a run keeps at
/// most 500 characters of it) and links.
const ITEM_KIND: usize = 100;
const ITEM_TITLE: usize = 500;
const ITEM_LINK: usize = 600;

/// The most characters a planning or coding run of this definition can be given, `blocks` being
/// the blocks it lists, in order: the planning context, and the coding context of each of its
/// repositories as a task's own (with every checkout) or as a planned child's (with its parent's
/// link), for an item with the longest kind, ID, title and links and the longest handover note.
pub(in crate::app) fn largest_context(
    workflow: &crate::workflows::Workflow,
    blocks: &[crate::workflows::Block],
    texts: &Texts,
) -> usize {
    let (kind, title, link) = (
        "x".repeat(ITEM_KIND),
        "x".repeat(ITEM_TITLE),
        "x".repeat(ITEM_LINK),
    );
    let subject = Subject {
        kind: &kind,
        id: u64::MAX,
        title: &title,
        url: &link,
        parent: Some(&link),
    };
    // A coding context is its opening, the note and the planning context (`coding_context`):
    // only the opening depends on the repository.
    let planning = instructions(workflow, blocks, texts).chars().count();
    // The longest note with the longer of its two prefaces (the person's or the supervisor's).
    let note = "x".repeat(record::MAX_HANDOVER);
    let mut rest = handover(&note, texts)
        .chars()
        .count()
        .max(drafted_handover(&note, texts).chars().count())
        .max(preapproved_handover(&note, texts).chars().count())
        + planning;
    // The sentences a workflow's settings add to every coding context.
    if workflow.allow.contains(&crate::workflows::Allow::WorkItems) {
        rest += dependency_links(texts).chars().count();
    }
    if workflow.delivery.dependency_policy == crate::workflows::DependencyPolicy::Stack {
        rest += stacked(
            &super::dependencies::Base {
                repository: String::new(),
                branch: "x".repeat(250),
                pull_request: u64::MAX,
                url: "x".repeat(ITEM_LINK),
            },
            texts,
        )
        .chars()
        .count();
    }
    checkouts(workflow)
        .into_iter()
        .flat_map(|(repository, directory)| {
            [
                task_checkouts(workflow, &repository, &directory),
                vec![(repository, directory)],
            ]
        })
        .map(|given| opening(&subject, &given, texts).chars().count() + rest)
        .fold(planning, usize::max)
}

/// The characters of the instructions and instruction blocks as a context carries them: what a
/// change to them adds to or takes from `largest_context`.
pub(in crate::app) fn instructions_part(
    workflow: &crate::workflows::Workflow,
    blocks: &[crate::workflows::Block],
    texts: &Texts,
) -> usize {
    let mut bare = workflow.clone();
    bare.instructions.clear();
    instructions(workflow, blocks, texts).chars().count()
        - instructions(&bare, &[], texts).chars().count()
}

async fn planning(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    workflow: &crate::workflows::Workflow,
    now: i64,
    paused: bool,
) -> std::result::Result<bool, String> {
    let Some(planned) = run.planning.clone() else {
        // The item's dependencies first: planning waits, without a slot, until they are met.
        if paused {
            return Ok(false);
        }
        if let Some(changed) = gate(host, azure, run, None, Phase::Planning, now).await? {
            return Ok(changed);
        }
        // Recorded before the machine is asked, so a restart asks for this same run.
        run.planning = Some(Implementation {
            id: uuid::Uuid::new_v4().to_string(),
            machine: workflow.machine.clone(),
            started: false,
            state: None,
            failure: None,
            time_limit_minutes: None,
        });
        return Ok(true);
    };
    if !planned.started {
        if paused {
            return Ok(false);
        }
        let detail = match azure
            .item(
                &host.state,
                run.item.source.as_deref(),
                Some(&run.item.project),
                run.item.id,
            )
            .await
        {
            Ok(detail) => detail,
            Err(error) => return wait(run, error),
        };
        let limit = time_limit(host, workflow, true);
        let launched = implementation::launch_planned(
            host,
            implementation::Planned {
                id: planned.id.clone(),
                machine: planned.machine.clone(),
                provider: workflow.agent.provider.clone(),
                model: workflow.agent.model.clone(),
                effort: workflow.agent.effort.clone(),
                directories: checkouts(workflow).into_iter().map(|(_, d)| d).collect(),
                context: fitted(instructions(workflow, &run.blocks, &prompt_texts(host)))?,
                source: run.item.source.clone(),
                detail,
                planning: true,
                allow: Vec::new(),
                round: false,
                resume: None,
                time_limit_minutes: limit,
            },
        )
        .await;
        return match launched {
            Ok(_) => {
                if let Some(p) = run.planning.as_mut() {
                    p.started = true;
                    p.time_limit_minutes = recorded_limit(limit);
                }
                run.error = None;
                Ok(true)
            }
            Err(error) if transient(&error.to_string()) => wait(run, error.to_string()),
            Err(error) => Err(format!("The planning run could not be launched: {error}")),
        };
    }
    let observed = match implementation::observe(host, &planned.machine, &planned.id).await {
        Ok(Some(observed)) => observed,
        Ok(None) => {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "Planning run {} is not on its machine; resume to plan again.",
                    planned.id
                ),
            );
            return Ok(true);
        }
        Err(error) => return wait(run, error.to_string()),
    };
    let observed_state = observed["state"].as_str().unwrap_or_default().to_owned();
    let changed = planned.state.as_deref() != Some(observed_state.as_str()) || run.error.is_some();
    run.error = None;
    set_observed(run, &observed_state);
    match observed_state.as_str() {
        "planning" => Ok(changed),
        "plan_ready" => {
            let children = observed["plan"]["children"].as_array().map_or(0, Vec::len);
            run.advance(
                Phase::PlanReview,
                now,
                &format!("plan_ready: {children} children proposed"),
            );
            run.attention = Some(format!(
                "Review the plan of planning run {} and confirm the children to create (implementation_create).",
                planned.id
            ));
            Ok(true)
        }
        "interrupted" => {
            run.stop(
                Phase::Interrupted,
                now,
                format!(
                    "Planning run {} was interrupted; resume to plan again.",
                    planned.id
                ),
            );
            Ok(true)
        }
        other => {
            let failure = observed_failure(&observed);
            if let Some(planning) = run.planning.as_mut() {
                planning.failure = failure;
            }
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "Planning run {} {} Read its record (implementation_runs), then resume to plan again.",
                    planned.id,
                    ended(other, failure)
                ),
            );
            Ok(true)
        }
    }
}

/// Judge the dependencies of the run's item (`child` none) or of one child, read now.
async fn judge_dependencies(
    host: &Arc<Host>,
    azure: &Azure,
    run: &Run,
    subject: u64,
) -> std::result::Result<
    (
        super::dependencies::Verdict,
        Vec<super::dependencies::Dependency>,
    ),
    String,
> {
    let graph = azure.dependencies(&host.state, run, subject).await?;
    if graph.direct.is_empty() && graph.foreign.is_empty() {
        return Ok((super::dependencies::Verdict::Met, Vec::new()));
    }
    let mut runs = host.state.workflows.db.list(&[], None, 500).map_err(fail)?;
    // This run as it is now: its children's pull requests may have just been read again.
    runs.retain(|r| r.id != run.id);
    runs.push(run.clone());
    let known = super::dependencies::known_pulls(&runs);
    // A catalog that cannot be read does not hold the run: each completed pull request then
    // counts whatever branch it went into.
    let defaults = azure
        .default_branches(&host.state, run)
        .await
        .unwrap_or_default();
    let delivery = run
        .definition
        .as_ref()
        .map(|d| d.delivery.clone())
        .unwrap_or_default();
    let target = delivery.target_branch.trim_start_matches("refs/heads/");
    Ok(super::dependencies::evaluate(
        &graph,
        &run.item.project,
        &known,
        &defaults,
        (
            (!target.is_empty()).then_some(target),
            delivery.dependency_policy == crate::workflows::DependencyPolicy::Stack,
        ),
        &run.skipped_dependencies,
    ))
}

/// Record the dependencies judged for `subject` on the run (the item's) or on its child.
fn record_dependencies(
    run: &mut Run,
    subject: u64,
    dependencies: Vec<super::dependencies::Dependency>,
) {
    run.dependencies_for = Some(subject);
    if let Some(child) = run.children.iter_mut().find(|c| c.id == subject) {
        child.dependencies = dependencies;
    } else {
        run.dependencies = dependencies;
    }
}

/// Before a planning run or a child's coding run is recorded: go on (none) when the work item's
/// dependencies are met and the workflow's limit of open pull requests leaves room, or wait in
/// `waiting_dependency`, or stop in `needs_attention` on what waiting cannot fix; returns
/// whether the run changed.
async fn gate(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    child: Option<usize>,
    stage: Phase,
    now: i64,
) -> std::result::Result<Option<bool>, String> {
    let subject = child.map_or(run.item.id, |i| run.children[i].id);
    if run.dependencies_met != Some(subject) {
        let judged = match judge_dependencies(host, azure, run, subject).await {
            Ok(judged) => judged,
            Err(error) => return wait(run, error).map(Some),
        };
        if let Some(changed) = settle(run, subject, stage, judged, now) {
            return Ok(Some(changed));
        }
    }
    // Planning opens no pull request: only a child's coding run counts against the limit.
    if child.is_none() {
        return Ok(None);
    }
    limit(host, azure, run, subject, stage, now).await
}

/// Apply a judgment of `subject`'s dependencies: none when they are met (the run goes on),
/// otherwise the run waits or stops; the change is recorded either way.
fn settle(
    run: &mut Run,
    subject: u64,
    stage: Phase,
    (verdict, dependencies): (
        super::dependencies::Verdict,
        Vec<super::dependencies::Dependency>,
    ),
    now: i64,
) -> Option<bool> {
    use super::dependencies::Verdict;
    // The open pull request a child is stacked on, when the workflow stacks.
    let base = dependencies.iter().find_map(|d| d.base.clone());
    record_dependencies(run, subject, dependencies);
    run.error = None;
    match verdict {
        Verdict::Met => {
            run.dependencies_met = Some(subject);
            if let Some(child) = run.children.iter_mut().find(|c| c.id == subject) {
                child.base = base;
            }
            None
        }
        Verdict::Waiting(text) => {
            let since = run
                .wait
                .as_ref()
                .filter(|w| w.subject == subject)
                .map_or(now, |w| w.since);
            run.wait = Some(super::dependencies::Wait {
                continue_to: stage,
                subject,
                since,
                checked_at: now,
                text,
                reason: "dependency".into(),
                repository: None,
            });
            if run.state != Phase::WaitingDependency {
                run.advance(Phase::WaitingDependency, now, "waiting for dependencies");
            }
            Some(true)
        }
        Verdict::Blocked(reason) => {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "{reason} Fix the links, then resume; or skip the dependency (workflow_run_skip_dependency)."
                ),
            );
            run.cause = Some("dependency".into());
            Some(true)
        }
    }
}

/// The workflow's limit of open pull requests per repository (`delivery.max_open_pull_requests`)
/// for the child `subject`: none when it leaves room (or there is no limit, or the person skipped
/// it), otherwise the run waits in `waiting_dependency` behind the older runs waiting for the
/// same repository.
async fn limit(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    subject: u64,
    stage: Phase,
    now: i64,
) -> std::result::Result<Option<bool>, String> {
    let workflow = run
        .definition
        .clone()
        .ok_or("The run has no workflow definition.")?;
    let max = workflow.delivery.max_open_pull_requests as usize;
    if max == 0 || run.limit_skipped == Some(subject) {
        return Ok(None);
    }
    // The repository the child will be coded in, as its coding run chooses it.
    let detail = match azure
        .item(
            &host.state,
            run.item.source.as_deref(),
            Some(&run.item.project),
            subject,
        )
        .await
    {
        Ok(detail) => detail,
        Err(error) => return wait(run, error).map(Some),
    };
    let Some((repository, _)) =
        named_repository(&workflow, &detail.item.title, &description(&detail))
    else {
        return Ok(None);
    };
    let id = repository.id.to_lowercase();
    // One run decides at a time, and a run that passes records it before the next decides.
    let _deciding = host.state.workflows.admission.lock().await;
    let mut runs = host.state.workflows.db.list(&[], None, 500).map_err(fail)?;
    // This run as it is now, in its place by creation; its own reservation does not count
    // against it.
    let mut own = run.clone();
    own.coding_into = None;
    match runs.iter_mut().find(|r| r.id == run.id) {
        Some(slot) => *slot = own,
        None => runs.insert(0, own),
    }
    let open = super::dependencies::open_pulls(&runs, &workflow.id, &id);
    let ahead = super::dependencies::queued_before(&runs, run, &id).map(|r| r.id.clone());
    if open.len() < max && ahead.is_none() {
        run.coding_into = Some(id);
        run.updated_at = now;
        host.state.workflows.db.save(run).map_err(fail)?;
        return Ok(None);
    }
    let text = match ahead {
        Some(ahead) if open.len() < max => format!(
            "Waiting behind workflow run {ahead}, created before it, for room among the open pull requests of {} (at most {max}).",
            repository.name
        ),
        _ => format!(
            "Waiting: {} pull requests of workflow {} are open or being coded in {} (at most {max}): {}.",
            open.len(),
            workflow.id,
            repository.name,
            open.join(", ")
        ),
    };
    let since = run
        .wait
        .as_ref()
        .filter(|w| w.subject == subject)
        .map_or(now, |w| w.since);
    run.wait = Some(super::dependencies::Wait {
        continue_to: stage,
        subject,
        since,
        checked_at: now,
        text,
        reason: "open_pull_requests".into(),
        repository: Some(id),
    });
    run.error = None;
    if run.state != Phase::WaitingDependency {
        run.advance(
            Phase::WaitingDependency,
            now,
            "waiting for open pull requests",
        );
    }
    Ok(Some(true))
}

/// A run waiting for its dependencies or for room among the open pull requests: judged again
/// every watch interval, it goes on to its stage once both allow it.
async fn waiting(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    now: i64,
) -> std::result::Result<bool, String> {
    let wait = run.wait.clone().ok_or("The run has no dependency wait.")?;
    let settings = host.state.settings.map().map_err(fail)?.workflow_settings;
    let days = i64::from(settings.dependency_wait_days);
    if now - wait.since >= days * 86_400 {
        run.stop(
            Phase::NeedsAttention,
            now,
            format!(
                "#{} waited more than {days} days ({}) Resume to wait again, or skip the wait (workflow_run_skip_dependency).",
                wait.subject, wait.text
            ),
        );
        run.cause = Some("dependency_timeout".into());
        return Ok(true);
    }
    if now - wait.checked_at < i64::from(settings.watch_interval_minutes) * 60 {
        return Ok(false);
    }
    // A child may wait for a sibling coded before it: the sibling's pull requests are read
    // again first, as the pull-request stage would (a failed read keeps the last ones).
    for index in 0..run.children.len() {
        if run.children[index].outcome.as_deref() == Some("done") {
            let _ = detect(host, azure, run, index).await;
        }
    }
    if let Some(changed) = conflicted(host, azure, run, now).await {
        return Ok(changed);
    }
    if let Some(started) = super::review::detect(host, azure, run, now).await {
        return Ok(started);
    }
    if run.dependencies_met != Some(wait.subject) {
        let judged = match judge_dependencies(host, azure, run, wait.subject).await {
            Ok(judged) => judged,
            Err(error) => {
                // Read again at the next interval.
                if let Some(w) = run.wait.as_mut() {
                    w.checked_at = now;
                }
                run.error = Some(error);
                return Ok(true);
            }
        };
        if settle(run, wait.subject, wait.continue_to, judged, now).is_some() {
            return Ok(true);
        }
    }
    let planning = wait.continue_to == Phase::Planning;
    if !planning
        && let Some(changed) = limit(host, azure, run, wait.subject, wait.continue_to, now).await?
    {
        return Ok(changed);
    }
    run.wait = None;
    let outcome = if wait.reason == "open_pull_requests" {
        "room among the open pull requests"
    } else {
        "dependencies met"
    };
    run.advance(wait.continue_to, now, outcome);
    Ok(true)
}

/// A coded child's open pull request that conflicts with its target branch (as Azure DevOps last
/// computed its merge status) stops the run in `needs_attention`, naming the files and, as the
/// likely cause, a pull request of another workflow run completed into the same branch.
/// Nothing is resolved here: the person decides (resume to wait on, or have the agent update
/// its branch). A conflict the person resumed past does not stop the run again until the
/// target branch moves.
async fn conflicted(host: &Arc<Host>, azure: &Azure, run: &mut Run, now: i64) -> Option<bool> {
    let (pull, key) = run
        .children
        .iter()
        .filter(|c| c.outcome.as_deref() == Some("done"))
        .flat_map(|c| &c.pull_requests)
        .filter(|p| p.status == "active" && p.merge_status.as_deref() == Some("conflicts"))
        .map(|p| {
            let key = format!("{}:{}", p.id, p.target_commit.as_deref().unwrap_or(""));
            (p.clone(), key)
        })
        .find(|(_, key)| !run.conflicts_accepted.contains(key))?;
    let files = azure
        .conflicts(&host.state, run, &pull.repository, pull.id)
        .await
        .unwrap_or_default();
    let target = pull.target.clone().unwrap_or_default();
    // The most recent pull request of another run completed into the same branch.
    let cause = host
        .state
        .workflows
        .db
        .list(&[], None, 500)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.id != run.id)
        .flat_map(|r| {
            r.children
                .into_iter()
                .flat_map(|c| c.pull_requests)
                .map(move |p| (r.item.url.clone(), p))
        })
        .filter(|(_, p)| {
            p.status == "completed"
                && p.repository.eq_ignore_ascii_case(&pull.repository)
                && p.target.as_deref().unwrap_or_default() == target
        })
        .max_by(|a, b| a.1.closed_at.cmp(&b.1.closed_at))
        .map(|(item, p)| format!("{} of {item}", p.url));
    let branch = target.trim_start_matches("refs/heads/");
    let text = format!(
        "Pull request {} conflicts with {}{}{}. Nothing was resolved: have its agent update its branch (or update it yourself), then resume to wait for it again.",
        pull.url,
        if branch.is_empty() {
            "its target branch"
        } else {
            branch
        },
        if files.is_empty() {
            String::new()
        } else {
            format!(" in {}", files.join(", "))
        },
        cause
            .as_deref()
            .map(|c| format!(", likely since pull request {c} was completed into it"))
            .unwrap_or_default()
    );
    run.conflict = Some(super::record::Conflict {
        pull_request: pull.id,
        url: pull.url.clone(),
        files,
        target_commit: pull.target_commit.clone(),
        cause,
        at: now,
    });
    let _ = key;
    run.stop(Phase::NeedsAttention, now, text);
    run.cause = Some("conflict".into());
    Some(true)
}

/// A child stacked on another's pull request (`stack` policy) whose own pull request still goes
/// into that branch once the other's is completed stops the run in `needs_attention`: it must
/// be retargeted to the branch the other went into.
fn retarget(host: &Arc<Host>, run: &mut Run, now: i64) -> Option<bool> {
    let runs = host
        .state
        .workflows
        .db
        .list(&[], None, 500)
        .unwrap_or_default();
    let completed = |url: &str| {
        runs.iter()
            .chain(std::iter::once(&*run))
            .flat_map(|r| r.children.iter().flat_map(|c| &c.pull_requests))
            .any(|p| p.url == url && p.status == "completed")
    };
    let (child, pull, base) = run.children.iter().find_map(|c| {
        let base = c.base.as_ref().filter(|_| !c.retarget_accepted)?;
        let pull = c.pull_requests.iter().find(|p| {
            p.status == "active"
                && p.target
                    .as_deref()
                    .map(|t| t.trim_start_matches("refs/heads/"))
                    == Some(base.branch.as_str())
        })?;
        completed(&base.url).then(|| (c.url.clone(), pull.url.clone(), base.clone()))
    })?;
    run.stop(
        Phase::NeedsAttention,
        now,
        format!(
            "Pull request {pull} of {child} was stacked on branch {}, whose pull request {} is now completed: retarget it to the branch that one went into (its source branch may be deleted), then resume.",
            base.branch, base.url
        ),
    );
    run.cause = Some("retarget".into());
    Some(true)
}

/// The children the ledger created, in order.
fn created_children(record: &Value) -> Vec<Child> {
    record["created"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let id = c["id"].as_u64().filter(|id| *id > 0)?;
            let url = c["url"].as_str().and_then(crate::tools::todo::https_link)?;
            Some(Child {
                id,
                title: c["title"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(500)
                    .collect(),
                url,
                kind: c["kind"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(120)
                    .collect(),
                ..Default::default()
            })
        })
        .collect()
}

async fn creating(host: &Arc<Host>, run: &mut Run, now: i64) -> std::result::Result<bool, String> {
    let ledger = run
        .ledger
        .clone()
        .ok_or("The run has no creation ledger entry.")?;
    let record = implementation::creation_record(host, &ledger)
        .await
        .ok_or("The creation ledger entry is missing.")?;
    let state = record["state"].as_str().unwrap_or_default().to_owned();
    match state.as_str() {
        "sending" => Ok(false),
        "created" => {
            run.children = created_children(&record);
            let count = run.children.len();
            run.advance(Phase::Coding, now, &format!("created: {count} children"));
            Ok(true)
        }
        _ if run.creation_accepted => {
            run.children = created_children(&record);
            let count = run.children.len();
            run.advance(
                Phase::Coding,
                now,
                &format!("{state}, accepted: {count} children created"),
            );
            Ok(true)
        }
        _ => {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "The creation of the children ended {state}; review what was created (implementation_creation {ledger}), then resume to code the children that were created."
                ),
            );
            Ok(true)
        }
    }
}

/// The plain text of the item's description.
pub(super) fn description(detail: &Detail) -> String {
    detail
        .fields
        .iter()
        .find(|f| f.reference == "System.Description")
        .and_then(|f| f.value.as_str())
        .map(crate::adapters::teams::plain_text)
        .unwrap_or_default()
}

/// The workflow repository a child names (the one named first in its title and description,
/// as a whole word), or the workflow's first repository.
pub(in crate::app) fn named_repository(
    workflow: &crate::workflows::Workflow,
    title: &str,
    description: &str,
) -> Option<(crate::workflows::Repository, PathBuf)> {
    let text = format!("{title}\n{description}").to_lowercase();
    // A name followed by a full stop is still a mention.
    let word = |c: char| c.is_alphanumeric() || "_-".contains(c);
    let position = |name: &str| {
        let name = name.to_lowercase();
        text.match_indices(&name)
            .find(|(at, _)| {
                let before = text[..*at].chars().next_back();
                let after = text[at + name.len()..].chars().next();
                !before.is_some_and(word) && !after.is_some_and(word)
            })
            .map(|(at, _)| at)
    };
    let all = checkouts(workflow);
    all.iter()
        .filter_map(|(r, d)| position(&r.name).map(|at| (at, r, d)))
        .min_by_key(|(at, _, _)| *at)
        .map(|(_, r, d)| (r.clone(), d.clone()))
        .or_else(|| all.into_iter().next())
}

/// The branch a coding session reported: the last line of its report that starts with
/// `Branch:`, as a plain branch name.
pub(in crate::app) fn reported_branch(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .find_map(|line| match branch_line(line)? {
            (None, branch) => Some(branch),
            (Some(_), _) => None,
        })
}

/// The other repositories' branches a coding session reported: for each repository named by a
/// `Branch REPOSITORY: NAME` line, the last such line's branch, as the report names them.
pub(in crate::app) fn reported_repository_branches(output: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for line in output.lines().rev() {
        if let Some((Some(repository), branch)) = branch_line(line)
            && !found
                .iter()
                .any(|(r, _)| r.eq_ignore_ascii_case(&repository))
        {
            found.push((repository, branch));
        }
    }
    found.reverse();
    found
}

/// One branch line of a report: `Branch: NAME` (no repository) or `Branch REPOSITORY: NAME`,
/// with a valid branch name and, when one is named, a repository of at most 100 characters
/// without control characters (what it names is checked against the workflow before use).
fn branch_line(line: &str) -> Option<(Option<String>, String)> {
    // A list marker or emphasis may open the line.
    let line = line.trim_start_matches(|c: char| c.is_whitespace() || "-*>`".contains(c));
    let word = "Branch";
    let rest = line
        .get(..word.len())
        .filter(|p| p.eq_ignore_ascii_case(word))
        .map(|_| &line[word.len()..])?;
    let (repository, rest) = if let Some(rest) = rest.strip_prefix(':') {
        (None, rest)
    } else if rest.starts_with(char::is_whitespace) {
        let (named, rest) = rest.split_once(':')?;
        let named = named.trim_matches(|c: char| c.is_whitespace() || "*`\"'".contains(c));
        if named.is_empty() || named.chars().count() > 100 || named.chars().any(char::is_control) {
            return None;
        }
        (Some(named.to_owned()), rest)
    } else {
        return None;
    };
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || "*`\"'".contains(c));
    let token = rest.split_whitespace().next()?;
    // Only a remark may follow the name («(pushed)», «— with tests»), never more words.
    let after = rest[token.len()..].trim_start();
    if !(after.is_empty() || after.starts_with(['(', '-', '\u{2013}', '\u{2014}'])) {
        return None;
    }
    // Sentence punctuation and quoting after the name; a trailing hyphen is the name's.
    let branch = token.trim_end_matches(|c: char| ".,;:)`\"'*".contains(c));
    let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    crate::ado::workflow::valid_branch(branch).then(|| (repository, branch.to_owned()))
}

/// The checkouts a task's coding run is given: its own repository's first (its working
/// directory), then the workflow's other checkouts in order, at most sixteen.
pub(super) fn task_checkouts(
    workflow: &crate::workflows::Workflow,
    primary: &crate::workflows::Repository,
    directory: &Path,
) -> Vec<(crate::workflows::Repository, PathBuf)> {
    std::iter::once((primary.clone(), directory.to_path_buf()))
        .chain(
            checkouts(workflow)
                .into_iter()
                .filter(|(r, _)| !r.id.eq_ignore_ascii_case(&primary.id)),
        )
        .take(16)
        .collect()
}

/// Record the branches a child's session reported. A task's lines for its other repositories
/// count only for a repository whose checkout the run was given, by the workflow's own name for
/// it; any other repository is noted as ignored, its name only when it is a plain one.
fn record_branches(
    child: &mut Child,
    workflow: &crate::workflows::Workflow,
    task: bool,
    output: &str,
) {
    child.branch = reported_branch(output);
    child.branches.clear();
    child.ignored_branches.clear();
    let (Some(primary), Some(directory)) = (child.repository.clone(), child.directory.clone())
    else {
        return;
    };
    if !task {
        return;
    }
    let given = task_checkouts(workflow, &primary, &directory);
    for (named, branch) in reported_repository_branches(output) {
        let Some((repository, _)) = given
            .iter()
            .find(|(r, _)| r.name.eq_ignore_ascii_case(&named))
        else {
            let plain = named.len() <= 64
                && named
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
            let note = if plain {
                named
            } else {
                "(not a plain repository name)".to_owned()
            };
            if !child.ignored_branches.contains(&note) && child.ignored_branches.len() < 10 {
                child.ignored_branches.push(note);
            }
            continue;
        };
        if repository.id.eq_ignore_ascii_case(&primary.id) {
            // `Branch PRIMARY: NAME` is the primary's line when there is no plain one.
            if child.branch.is_none() {
                child.branch = Some(branch);
            }
            continue;
        }
        child.branches.push(record::RepositoryBranch {
            repository: repository.clone(),
            branch,
        });
    }
}

async fn coding(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    workflow: &crate::workflows::Workflow,
    now: i64,
    paused: bool,
) -> std::result::Result<bool, String> {
    let Some(index) = run.children.iter().position(|c| c.outcome.is_none()) else {
        let done = run.children.iter().filter(|c| !c.accepted()).count();
        run.advance(
            Phase::PullRequests,
            now,
            &format!("coded: {done} of {}", run.children.len()),
        );
        run.waiting_since = Some(now);
        run.checked_at = None;
        return Ok(true);
    };
    let parent = (!run.task()).then(|| run.item.url.clone());
    let child = run.children[index].clone();
    let Some(coding) = child.coding.clone() else {
        // The child's dependencies first (a task's are its own): it waits, without a slot,
        // until they are met.
        if paused {
            return Ok(false);
        }
        if let Some(changed) = gate(host, azure, run, Some(index), Phase::Coding, now).await? {
            return Ok(changed);
        }
        // Recorded before the machine is asked: never a second run for this child.
        run.children[index].coding = Some(Implementation {
            id: uuid::Uuid::new_v4().to_string(),
            machine: workflow.machine.clone(),
            started: false,
            state: None,
            failure: None,
            time_limit_minutes: None,
        });
        return Ok(true);
    };
    if !coding.started {
        if paused {
            return Ok(false);
        }
        let detail = match azure
            .item(
                &host.state,
                run.item.source.as_deref(),
                Some(&run.item.project),
                child.id,
            )
            .await
        {
            Ok(detail) => detail,
            Err(error) => return wait(run, error),
        };
        let (repository, directory) =
            named_repository(workflow, &detail.item.title, &description(&detail))
                .ok_or("The workflow has no checkout.")?;
        // A task may need several of the workflow's repositories: its run is given every
        // checkout, its own repository's first. A planned child stays in the one the plan named.
        let given = if run.task() {
            task_checkouts(workflow, &repository, &directory)
        } else {
            vec![(repository.clone(), directory.clone())]
        };
        let subject = Subject {
            kind: &child.kind,
            id: child.id,
            title: &child.title,
            url: &child.url,
            parent: parent.as_deref(),
        };
        let note = run.handover.as_deref().map(|text| {
            if run.handover_drafted && run.preapproved_restart {
                Note::Preapproved(text)
            } else if run.handover_drafted {
                Note::Drafted(text)
            } else {
                Note::Person(text)
            }
        });
        let texts = prompt_texts(host);
        let mut context = coding_context(&subject, &given, note, workflow, &run.blocks, &texts);
        if let Some(base) = &child.base {
            context.push_str(&stacked(base, &texts));
        }
        let context = fitted(context)?;
        let limit = time_limit(host, workflow, false);
        let launched = implementation::launch_planned(
            host,
            implementation::Planned {
                id: coding.id.clone(),
                machine: coding.machine.clone(),
                provider: workflow.agent.provider.clone(),
                model: workflow.agent.model.clone(),
                effort: workflow.agent.effort.clone(),
                directories: given.into_iter().map(|(_, d)| d).collect(),
                context,
                source: run.item.source.clone(),
                detail,
                planning: false,
                // As the run's definition holds it: later edits do not change this run.
                allow: workflow.allow.clone(),
                round: false,
                resume: None,
                time_limit_minutes: limit,
            },
        )
        .await;
        return match launched {
            Ok(_) => {
                // Its coding run counts as open from now on: the reservation ends.
                run.coding_into = None;
                let child = &mut run.children[index];
                if let Some(c) = child.coding.as_mut() {
                    c.started = true;
                    c.time_limit_minutes = recorded_limit(limit);
                }
                child.repository = Some(repository);
                child.directory = Some(directory);
                child.started_at = Some(now);
                run.error = None;
                Ok(true)
            }
            Err(error) if transient(&error.to_string()) => wait(run, error.to_string()),
            Err(error) => Err(format!(
                "The coding run of {} could not be launched: {error}",
                child.url
            )),
        };
    }
    let observed = match implementation::observe(host, &coding.machine, &coding.id).await {
        Ok(Some(observed)) => observed,
        Ok(None) => {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "Coding run {} of {} is not on its machine.",
                    coding.id, child.url
                ),
            );
            return Ok(true);
        }
        Err(error) => return wait(run, error.to_string()),
    };
    let observed_state = observed["state"].as_str().unwrap_or_default().to_owned();
    let changed = coding.state.as_deref() != Some(observed_state.as_str()) || run.error.is_some();
    run.error = None;
    set_observed(run, &observed_state);
    // What its session found so far is kept as soon as it is read.
    super::findings::record(&host.state, run, index, &observed);
    match observed_state.as_str() {
        "running" => Ok(changed),
        "done" => {
            let task = run.task();
            {
                let child = &mut run.children[index];
                // Its delivery reports read as the branch lines a report ends with, after them.
                let output = with_reports(child, &observed);
                record_branches(child, workflow, task, &output);
                child.outcome = Some("done".into());
                child.finished_at = Some(now);
            }
            // A report without its branch: what the session delivered is looked for in the
            // repositories and the Wiki, for the person to accept; a Wiki page it reported and
            // the user changed while it coded is its delivery.
            if !run.children[index].reported() {
                let found = super::found::discover(host, azure, run, index, now).await;
                let reported = reported_pages(&observed);
                let child = &mut run.children[index];
                child.wiki_pages = found
                    .wiki_pages
                    .iter()
                    .filter(|p| reported.contains(&page_key(&p.path)))
                    .cloned()
                    .collect();
                child.found = Some(found);
            }
            // The pull request the session opened, read once now; the next stage reads again.
            let _ = detect(host, azure, run, index).await;
            Ok(true)
        }
        other => {
            let failure = observed_failure(&observed);
            if let Some(coding) = run.children[index].coding.as_mut() {
                coding.failure = failure;
            }
            let found = super::found::discover(host, azure, run, index, now).await;
            let found_text = super::found::describe(&run.children[index].url, &found);
            run.children[index].found = Some(found);
            let continued = if failure.is_some_and(|f| f.cause.continuable()) {
                " If you continue its session to its final report, the run goes on with that report by itself."
            } else {
                ""
            };
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "Coding run {} of {} {} Read its record (implementation_runs), finish or fix the child, then resume to continue with the next child (this run is not started again).{continued}{found_text}",
                    coding.id,
                    child.url,
                    ended(other, failure)
                ),
            );
            Ok(true)
        }
    }
}

/// An observed coding run's final text followed by its delivery reports as the report lines
/// they stand for, so a reported branch counts, and wins, like a line of the report.
fn with_reports(child: &Child, observed: &Value) -> String {
    format!(
        "{}{}",
        observed["output"].as_str().unwrap_or_default(),
        reported_lines(child, observed)
    )
}

/// A coding run's delivery reports (`observed["reports"]`) as report lines: `Branch: NAME` for
/// the child's own repository, `Branch REPOSITORY: NAME` for another, checked like any line.
fn reported_lines(child: &Child, observed: &Value) -> String {
    let own = child.repository.as_ref().map(|r| r.name.to_lowercase());
    observed["reports"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["kind"] == "delivery")
        .filter_map(|r| {
            let (repository, branch) = (r["repository"].as_str()?, r["branch"].as_str()?);
            if branch.is_empty() {
                return None;
            }
            Some(
                if own.as_deref() == Some(repository.to_lowercase().as_str()) {
                    format!("\nBranch: {branch}")
                } else {
                    format!("\nBranch {repository}: {branch}")
                },
            )
        })
        .collect()
}

/// The Wiki pages a coding run's delivery reports name, as lowercase paths without the leading
/// `/`.
fn reported_pages(observed: &Value) -> std::collections::BTreeSet<String> {
    observed["reports"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["kind"] == "delivery")
        .flat_map(|r| r["wiki_pages"].as_array().cloned().unwrap_or_default())
        .filter_map(|p| p.as_str().map(page_key))
        .collect()
}

/// A Wiki page's path compared loosely: as the Wiki shows it (`/Deploy the portal`) or as its
/// repository stores it (`Deploy-the-portal.md`, `%2D` for a title's own `-`).
fn page_key(path: &str) -> String {
    let path = path.trim().trim_start_matches('/');
    let path = path.strip_suffix(".md").unwrap_or(path);
    url::form_urlencoded::parse(format!("k={}", path.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Read the pull requests of each branch a child reported and record them, keeping what the
/// user accepted; returns whether they changed.
pub(super) async fn detect(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    index: usize,
) -> std::result::Result<bool, String> {
    let delivered = run.children[index].delivered();
    if delivered.is_empty() {
        return Ok(false);
    }
    let mut found = Vec::new();
    for (repository, branch) in &delivered {
        found.extend(
            azure
                .pull_requests(&host.state, run, repository, branch)
                .await?,
        );
    }
    let child = &mut run.children[index];
    for pull in &mut found {
        pull.accepted = child.pull_requests.iter().any(|p| {
            p.id == pull.id && p.repository.eq_ignore_ascii_case(&pull.repository) && p.accepted
        });
    }
    if child.pull_requests == found {
        return Ok(false);
    }
    child.pull_requests = found;
    Ok(true)
}

async fn pull_requests(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    now: i64,
    poll: i64,
) -> std::result::Result<bool, String> {
    let mut changed = false;
    if let Some(child) = run
        .children
        .iter()
        .find(|c| c.outcome.as_deref() == Some("done") && !c.reported())
    {
        let found_text = child
            .found
            .as_ref()
            .map(|found| super::found::describe(&child.url, found))
            .unwrap_or_default();
        run.stop(
            Phase::NeedsAttention,
            now,
            format!(
                "The coding session of {} reported no branch, so its pull request cannot be found; open it by hand if it is missing, then resume.{found_text}",
                child.url
            ),
        );
        return Ok(true);
    }
    if run.checked_at.is_none_or(|at| now - at >= poll) {
        for index in 0..run.children.len() {
            if run.children[index].outcome.as_deref() != Some("done") {
                continue;
            }
            if let Err(error) = detect(host, azure, run, index).await {
                // A failed read waits for the poll interval too.
                run.checked_at = Some(now);
                run.error = Some(error);
                return Ok(true);
            }
        }
        // Recorded on every read, so the next read waits for the poll interval.
        run.checked_at = Some(now);
        run.error = None;
        changed = true;
        if let Some(stopped) = conflicted(host, azure, run, now).await {
            return Ok(stopped);
        }
        if let Some(stopped) = retarget(host, run, now) {
            return Ok(stopped);
        }
        // A reviewer asked for changes: the child's agent corrects it in a review round.
        if let Some(started) = super::review::detect(host, azure, run, now).await {
            return Ok(started);
        }
    }
    let coded: Vec<&Child> = run
        .children
        .iter()
        .filter(|c| c.outcome.as_deref() == Some("done"))
        .collect();
    if coded.is_empty() && !run.undelivered_accepted {
        let found_text: String = run
            .children
            .iter()
            .filter_map(|c| Some(super::found::describe(&c.url, c.found.as_ref()?)))
            .collect();
        let reason = format!(
            "No child was delivered ({} children, none coded to done), so there is no pull request to wait for; cancel the run, or resume it to finish it as done without delivering anything.{found_text}",
            run.children.len()
        );
        run.stop(Phase::NeedsAttention, now, reason);
        run.cause = Some("nothing_delivered".into());
        return Ok(true);
    }
    if let Some(pull) = coded
        .iter()
        .flat_map(|c| c.pull_requests.iter())
        .find(|p| p.status == "abandoned" && !p.accepted)
    {
        let reason = format!(
            "Pull request {} was abandoned; resume to continue without it.",
            pull.url
        );
        run.stop(Phase::NeedsAttention, now, reason);
        return Ok(true);
    }
    // Every branch of every coded child has its pull request, each completed (or abandoned and
    // accepted).
    // A child that delivered only Wiki pages waits for no pull request.
    let settled = coded.iter().all(|c| {
        c.wiki_only()
            || !c.pull_requests.is_empty()
                && c.without_pull_request().is_empty()
                && c.pull_requests
                    .iter()
                    .all(|p| p.status == "completed" || p.accepted)
    });
    if settled {
        let count = coded.iter().map(|c| c.pull_requests.len()).sum::<usize>();
        let outcome = if coded.is_empty() {
            "accepted: nothing delivered".to_owned()
        } else {
            format!("completed: {count} pull requests")
        };
        run.advance(Phase::Blocks, now, &outcome);
        return Ok(true);
    }
    let days = host
        .state
        .settings
        .map()
        .map_err(fail)?
        .workflow_settings
        .pull_request_wait_days;
    let since = run.waiting_since.unwrap_or(now);
    if now - since >= i64::from(days) * 86_400 {
        let missing: Vec<String> = coded
            .iter()
            .filter_map(|c| {
                let without = c.without_pull_request();
                if c.wiki_only() {
                    None
                } else if c.pull_requests.is_empty() {
                    Some(c.url.clone())
                } else if !without.is_empty() {
                    Some(format!("{} ({})", c.url, without.join(", ")))
                } else {
                    None
                }
            })
            .collect();
        let open: Vec<&str> = coded
            .iter()
            .flat_map(|c| c.pull_requests.iter())
            .filter(|p| p.status != "completed" && !p.accepted)
            .map(|p| p.url.as_str())
            .collect();
        run.stop(
            Phase::NeedsAttention,
            now,
            format!(
                "The pull requests were not completed within {days} days (open: {}; children without one: {}); resume to wait again.",
                if open.is_empty() { "none".into() } else { open.join(", ") },
                if missing.is_empty() { "none".into() } else { missing.join(", ") }
            ),
        );
        return Ok(true);
    }
    Ok(changed)
}

/// The children accepted without delivery whose coding session stopped while it worked
/// (`Cause::continuable`) and that the person has since continued to a final report that names
/// its branches: each is delivered now (`done`, with its branches), as if its session had ended
/// so. Its machine is asked to recover a report it did not follow. Returns what changed.
async fn recover_continued(host: &Arc<Host>, run: &mut Run, now: i64) -> Vec<Value> {
    let Some(workflow) = run.definition.clone() else {
        return Vec::new();
    };
    let task = run.task();
    let mut recovered = Vec::new();
    for index in 0..run.children.len() {
        let child = &run.children[index];
        let Some(coding) = child
            .coding
            .clone()
            .filter(|c| c.started && c.failure.is_some_and(|f| f.cause.continuable()))
        else {
            continue;
        };
        if !child.accepted() {
            continue;
        }
        let Ok(Some(observed)) =
            implementation::observe_recovering(host, &coding.machine, &coding.id).await
        else {
            continue;
        };
        if observed["state"] != "done" {
            continue;
        }
        let output = with_reports(child, &observed);
        let Some(delivered) = continued_delivery(child, &workflow, task, &output, now) else {
            continue;
        };
        recovered.push(json!({"child": delivered.url, "coding_run": coding.id, "continued": true}));
        run.children[index] = delivered;
    }
    recovered
}

/// The child as a continued session delivered it: `done` with the branches its final report
/// names, its coding run `done` without its failure. None when the report names no branch: a
/// session that ended without delivering is left for the person, who may continue it again.
fn continued_delivery(
    child: &Child,
    workflow: &crate::workflows::Workflow,
    task: bool,
    output: &str,
    now: i64,
) -> Option<Child> {
    let mut delivered = child.clone();
    record_branches(&mut delivered, workflow, task, output);
    if !delivered.reported() {
        return None;
    }
    delivered.outcome = Some("done".into());
    delivered.finished_at = Some(now);
    if let Some(c) = delivered.coding.as_mut() {
        c.state = Some("done".into());
        c.failure = None;
    }
    Some(delivered)
}

/// How long after a run stopped its continued session is watched (`watch_continued`): three
/// days. Later, resuming the run by hand still recovers a report the session wrote.
const CONTINUED_WATCH: i64 = 3 * 24 * 60 * 60;

/// Whether a run waiting for the person is due a look at its continued session: stopped in its
/// coding stage or because nothing was delivered, within `CONTINUED_WATCH` of its stop and not
/// read in the last `poll` seconds. Decided before any task is spawned or machine called.
fn continued_due(run: &Run, now: i64, poll: i64) -> bool {
    let nothing_delivered = run.cause.as_deref() == Some("nothing_delivered");
    let coding = run.stopped_in == Some(Phase::Coding) && run.cause.is_none();
    (nothing_delivered || coding)
        && now - run.updated_at < CONTINUED_WATCH
        && run.checked_at.is_none_or(|at| now - at >= poll)
}

/// A run stopped on a coding session that did not end with its answer, while it waits for the
/// person: the session they continue to a final report that names its branches
/// (`Cause::continuable`) is delivered by itself, read at most every `poll` seconds. Stopped in
/// its coding stage, the child is `done` with its report and the run codes on; stopped because nothing was delivered, the branches
/// it reports are waited for. Returns whether the run changed.
async fn watch_continued(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    now: i64,
    poll: i64,
) -> bool {
    if !continued_due(run, now, poll) {
        return false;
    }
    let nothing_delivered = run.cause.as_deref() == Some("nothing_delivered");
    let watched = |c: &Child| {
        c.coding
            .as_ref()
            .is_some_and(|i| i.started && i.failure.is_some_and(|f| f.cause.continuable()))
    };
    if nothing_delivered {
        if !run.children.iter().any(|c| c.accepted() && watched(c)) {
            return false;
        }
        run.checked_at = Some(now);
        let recovered = recover_continued(host, run, now).await;
        if recovered.is_empty() {
            return true;
        }
        run.advance(Phase::PullRequests, now, "continued");
        run.waiting_since = Some(now);
        run.checked_at = None;
        return true;
    }
    let Some(index) = run.children.iter().position(|c| c.outcome.is_none()) else {
        return false;
    };
    let Some(coding) = run.children[index]
        .coding
        .clone()
        .filter(|_| watched(&run.children[index]))
    else {
        return false;
    };
    run.checked_at = Some(now);
    let Ok(Some(observed)) =
        implementation::observe_recovering(host, &coding.machine, &coding.id).await
    else {
        return true;
    };
    if observed["state"] != "done" {
        return true;
    }
    let (Some(workflow), task) = (run.definition.clone(), run.task()) else {
        return true;
    };
    let output = with_reports(&run.children[index], &observed);
    let Some(delivered) = continued_delivery(&run.children[index], &workflow, task, &output, now)
    else {
        return true;
    };
    run.children[index] = delivered;
    let _ = detect(host, azure, run, index).await;
    run.advance(Phase::Coding, now, "continued");
    run.checked_at = None;
    true
}

/// Where a stopped run continues: the stage it stopped in, with what the person resumed past
/// accepted. A plan is never submitted again and a coding run recorded as started is never
/// started again; a planning run that did not end ready is replaced by a new one (planning is
/// read-only).
pub(in crate::app) async fn resume(host: &Arc<Host>, run: &mut Run, now: i64) -> Result<Value> {
    let mut stage = run
        .stopped_in
        .context("This run did not stop in a stage it can resume.")?;
    let mut accepted = Vec::new();
    match (stage, run.cause.as_deref()) {
        // The ledger entry names another parent: plan again, with a new planning run.
        (_, Some("ledger_parent")) => {
            if let Some(planned) = run.planning.take() {
                accepted.push(json!({"planning_run": planned.id, "replaced": true}));
            }
            run.ledger = None;
            stage = Phase::Planning;
        }
        // A conflict the person looked at: the run waits on, and that conflict against that
        // target commit does not stop it again.
        (_, Some("conflict")) => {
            if let Some(conflict) = run.conflict.take() {
                run.conflicts_accepted.push(format!(
                    "{}:{}",
                    conflict.pull_request,
                    conflict.target_commit.as_deref().unwrap_or("")
                ));
                accepted.push(json!({"conflict": conflict.url}));
            }
            run.waiting_since = Some(now);
            run.checked_at = None;
        }
        // A stacked pull request the person retargeted (or decided to keep).
        (_, Some("retarget")) => {
            for child in run.children.iter_mut().filter(|c| c.base.is_some()) {
                child.retarget_accepted = true;
                accepted.push(json!({"retargeted": child.url}));
            }
            run.checked_at = None;
        }
        // Nothing was delivered: unless a coding session the person continued has reported its
        // branches since (they are then waited for), the person accepts the run as done
        // without pull requests.
        (_, Some("nothing_delivered")) => {
            let recovered = recover_continued(host, run, now).await;
            if recovered.is_empty() {
                run.undelivered_accepted = true;
                accepted.push(json!({"nothing_delivered": true}));
            } else {
                accepted.extend(recovered);
                run.waiting_since = Some(now);
                run.checked_at = None;
            }
        }
        (Phase::Planning, _) => {
            if let Some(planned) = run.planning.clone()
                && planned.started
            {
                // A planning run the machine no longer has, or one that did not end ready, is
                // replaced (planning is read-only); one the machine cannot be asked about now
                // is looked at again by the stage.
                let replace =
                    match implementation::observe(host, &planned.machine, &planned.id).await {
                        Ok(Some(observed)) => {
                            !matches!(observed["state"].as_str(), Some("planning" | "plan_ready"))
                        }
                        Ok(None) => true,
                        Err(_) => !matches!(
                            planned.state.as_deref(),
                            None | Some("planning" | "plan_ready")
                        ),
                    };
                if replace {
                    accepted.push(json!({"planning_run": planned.id, "replaced": true}));
                    run.planning = None;
                }
            }
        }
        (Phase::CreatingChildren, _) => {
            run.creation_accepted = true;
            accepted.push(json!({"creation": run.ledger}));
        }
        (Phase::Coding, _) => {
            if let Some(index) = run.children.iter().position(|c| c.outcome.is_none())
                && let Some(coding) = run.children[index].coding.clone()
                && coding.started
            {
                // A coding run is never started again: one that ended (or that its machine no
                // longer has) is accepted as it ended, and the next child is coded. One whose
                // session the person continued to its final report ended `done` and is
                // delivered like any other.
                let (ended, output) =
                    match implementation::observe_recovering(host, &coding.machine, &coding.id)
                        .await
                    {
                        Ok(Some(observed)) => (
                            observed["state"].as_str().map(str::to_owned),
                            Some(with_reports(&run.children[index], &observed)),
                        ),
                        Ok(None) => (Some("missing".to_owned()), None),
                        Err(_) => (coding.state.clone(), None),
                    };
                if ended.as_deref() != Some("running") {
                    let (workflow, task) = (run.definition.clone(), run.task());
                    let child = &mut run.children[index];
                    let done = ended.as_deref() == Some("done");
                    child.outcome = Some(if done { "done" } else { "accepted" }.into());
                    child.finished_at = Some(now);
                    if done && let Some(c) = child.coding.as_mut() {
                        c.failure = None;
                    }
                    match &workflow {
                        Some(workflow) => {
                            record_branches(child, workflow, task, output.as_deref().unwrap_or(""))
                        }
                        None => child.branch = output.as_deref().and_then(reported_branch),
                    }
                    if let (Some(c), Some(ended)) = (child.coding.as_mut(), ended.clone()) {
                        c.state = Some(ended);
                    }
                    accepted
                        .push(json!({"child": child.url, "coding_run": coding.id, "ended": ended}));
                }
            }
        }
        // A review round: one that ended is accepted as it ended and the run returns to the
        // stage it came from; one whose session did not run to its end starts it again.
        (Phase::AddressingReview, _) => {
            if let Some((back, value)) = super::review::resume(run) {
                stage = back;
                accepted.push(value);
                if back == Phase::PullRequests {
                    run.checked_at = None;
                }
            }
        }
        // Waiting for its dependencies: the wait starts again, judged at once.
        (Phase::WaitingDependency, _) => {
            if let Some(wait) = run.wait.as_mut() {
                wait.since = now;
                wait.checked_at = 0;
            }
            accepted.push(json!({"dependencies": "waited for again"}));
        }
        (Phase::PullRequests, _) => {
            for child in &mut run.children {
                if child.outcome.as_deref() == Some("done") && !child.reported() {
                    child.outcome = Some("accepted".into());
                    accepted.push(json!({"child": child.url, "without": "pull_request"}));
                }
                for pull in &mut child.pull_requests {
                    if pull.status == "abandoned" && !pull.accepted {
                        pull.accepted = true;
                        accepted.push(json!({"pull_request": pull.url}));
                    }
                }
            }
            run.waiting_since = Some(now);
            run.checked_at = None;
        }
        _ => {}
    }
    run.advance(stage, now, "resumed");
    run.error = None;
    Ok(json!(accepted))
}

/// The tracked task of the control loop: one tick every 30 seconds.
pub(in crate::app) async fn schedule(host: Arc<Host>) {
    loop {
        let now = chrono::Utc::now().timestamp();
        if tick(&host, &Azure::Live, now, PULL_REQUEST_POLL_SECONDS)
            .await
            .is_err()
        {
            tracing::warn!(event = "workflow_engine_not_ready");
        }
        // The supervisor looks after the runs the engine just advanced (nothing when it is off).
        if super::supervisor::tick(&host, &Azure::Live, now)
            .await
            .is_err()
        {
            tracing::warn!(event = "workflow_supervisor_not_ready");
        }
        tokio::time::sleep(std::time::Duration::from_secs(TICK_SECONDS)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::{Repository, tests as fixtures};

    #[test]
    fn the_largest_context_is_the_longest_context_the_engine_builds() {
        let mut workflow = fixtures::workflow("payments");
        workflow.repositories.push(Repository {
            id: "1a2b3c4d-5b1d-4f7a-9c8e-0f3c6a2e5e6f".into(),
            name: "payments-web-with-a-longer-name".into(),
        });
        workflow.checkouts.insert(
            "1a2b3c4d-5b1d-4f7a-9c8e-0f3c6a2e5e6f".into(),
            PathBuf::from("/work/payments-web-with-a-longer-name"),
        );
        let blocks = vec![fixtures::instruction("base")];
        workflow.blocks = vec!["base".into()];
        let (kind, title, link) = (
            "x".repeat(ITEM_KIND),
            "x".repeat(ITEM_TITLE),
            "x".repeat(ITEM_LINK),
        );
        let subject = Subject {
            kind: &kind,
            id: u64::MAX,
            title: &title,
            url: &link,
            parent: Some(&link),
        };
        let note = "x".repeat(record::MAX_HANDOVER);
        let built = checkouts(&workflow)
            .into_iter()
            .flat_map(|(repository, directory)| {
                [
                    task_checkouts(&workflow, &repository, &directory),
                    vec![(repository, directory)],
                ]
            })
            .flat_map(|given| {
                [
                    Note::Person(&note),
                    Note::Drafted(&note),
                    Note::Preapproved(&note),
                ]
                .map(|preface| {
                    coding_context(
                        &subject,
                        &given,
                        Some(preface),
                        &workflow,
                        &blocks,
                        &Texts::default(),
                    )
                })
            })
            .map(|context| context.chars().count())
            .max()
            .unwrap();
        assert_eq!(
            largest_context(&workflow, &blocks, &Texts::default()),
            built
        );
        // The instructions and blocks take what their sections take, nothing else.
        let part = instructions_part(&workflow, &blocks, &Texts::default());
        let block = &blocks[0];
        assert_eq!(
            part,
            15 + workflow.instructions.chars().count()
                + 23
                + block.name.chars().count()
                + block.text.chars().count()
        );
        // A context that does not fit is refused whole, never cut.
        assert_eq!(fitted("x".repeat(CONTEXT)).unwrap().len(), CONTEXT);
        assert!(
            fitted("x".repeat(CONTEXT + 1))
                .unwrap_err()
                .contains("never cut")
        );
    }
}
