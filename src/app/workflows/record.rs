//! Workflow runs: the durable record of one execution of a workflow for one work item, kept in
//! `implementation/workflows.db` next to `runs.db`, on the coordinator only.
//!
//! The table `workflow_runs` holds each run under a stable UUID, indexed by its item's verified
//! link, its workflow, its state and when it changed; the payload is the whole record (`Run`):
//! the workflow's definition and blocks as they were when the run got its workflow, the item's
//! identity and link, the stage timeline, the implementation runs it asked for (the planning run
//! and one coding run per child), the creation ledger entry, the children with their branches
//! and pull requests, the block outcomes, the summaries and the attention or error text. A
//! partial unique index keeps one active run (any state but `done` and `cancelled`) per item.
use super::*;
use crate::ado::workflow::PullRequest;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(in crate::app) enum State {
    /// The workflow director is choosing the workflow.
    Classifying,
    /// No workflow yet: the director chose none, or there is no director; the user assigns one.
    Unassigned,
    /// The planning run proposes the children.
    Planning,
    /// The plan waits for the user's review and confirmation (`implementation_create`).
    PlanReview,
    /// The confirmed children are being created (the creation ledger).
    CreatingChildren,
    /// One coding run per child, one after another.
    Coding,
    /// Waiting for every pull request to be completed.
    PullRequests,
    /// The stage blocks.
    Blocks,
    Done,
    /// The engine could not continue (a refused launch, an unreadable item).
    Failed,
    Cancelled,
    /// The host restarted while an implementation run of this run was active.
    Interrupted,
    /// Stopped on something the user must look at (`attention` says what).
    NeedsAttention,
    /// Waiting, without a slot, for the dependencies of its item or of the next child to be
    /// met (`dependencies`); it goes on by itself.
    WaitingDependency,
    /// A reviewer asked for changes on a pull request: the agent of the child corrects it in a
    /// review round (`Child::rounds`), in the same branch and pull request, and sends it back.
    AddressingReview,
}
impl State {
    pub(in crate::app) const ALL: [State; 15] = [
        State::Classifying,
        State::Unassigned,
        State::Planning,
        State::PlanReview,
        State::CreatingChildren,
        State::Coding,
        State::PullRequests,
        State::Blocks,
        State::Done,
        State::Failed,
        State::Cancelled,
        State::Interrupted,
        State::NeedsAttention,
        State::WaitingDependency,
        State::AddressingReview,
    ];
    pub(in crate::app) fn word(self) -> &'static str {
        match self {
            State::Classifying => "classifying",
            State::Unassigned => "unassigned",
            State::Planning => "planning",
            State::PlanReview => "plan_review",
            State::CreatingChildren => "creating_children",
            State::Coding => "coding",
            State::PullRequests => "pull_requests",
            State::Blocks => "blocks",
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
            State::Interrupted => "interrupted",
            State::NeedsAttention => "needs_attention",
            State::WaitingDependency => "waiting_dependency",
            State::AddressingReview => "addressing_review",
        }
    }
    pub(in crate::app) fn parse(word: &str) -> Option<State> {
        State::ALL.into_iter().find(|s| s.word() == word)
    }
    /// Done or cancelled: nothing more happens to the run.
    pub(in crate::app) fn finished(self) -> bool {
        matches!(self, State::Done | State::Cancelled)
    }
    /// A run that ends here stops every agent session its implementation runs left alive.
    pub(in crate::app) fn stops_sessions(self) -> bool {
        matches!(self, State::Failed | State::Cancelled | State::Interrupted)
    }
    /// Stopped until the user resumes it.
    pub(in crate::app) fn stopped(self) -> bool {
        matches!(
            self,
            State::Failed | State::Interrupted | State::NeedsAttention
        )
    }
    /// Waiting on the user: Home lists these as attention items.
    pub(in crate::app) fn waits_on_user(self) -> bool {
        matches!(
            self,
            State::PlanReview
                | State::Unassigned
                | State::NeedsAttention
                | State::Interrupted
                | State::Failed
        )
    }
    /// The stages the engine advances by itself.
    pub(in crate::app) fn driven(self) -> bool {
        matches!(
            self,
            State::Classifying
                | State::Planning
                | State::PlanReview
                | State::CreatingChildren
                | State::Coding
                | State::PullRequests
                | State::Blocks
                | State::WaitingDependency
                | State::AddressingReview
        )
    }
    /// The stages that take one of the `concurrency` slots: those where a coding or planning
    /// agent works or the run is about to act. Waiting for the user's review or for pull
    /// requests takes none, and neither does classifying (a short read-only director session,
    /// bounded on its own by the engine).
    pub(in crate::app) fn takes_slot(self) -> bool {
        matches!(
            self,
            State::Planning
                | State::CreatingChildren
                | State::Coding
                | State::Blocks
                | State::AddressingReview
        )
    }
}

/// The work item a run is for, as it was read when the run started.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Item {
    pub provider: String,
    pub id: u64,
    pub project: String,
    /// The organization's URL (`https://dev.azure.com/NAME`).
    pub organization: String,
    /// The verified link.
    pub url: String,
    pub title: String,
    pub kind: String,
    /// `requirement`, `bug` or `task`.
    pub category: String,
    pub state: String,
    /// The Azure source it was read from, when several are enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// One stage of the timeline.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Stage {
    pub stage: State,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// An implementation run the workflow run asked for, by the ID it chose and recorded first.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Implementation {
    pub id: String,
    pub machine: String,
    /// The machine accepted the launch; from then on the run is never started again.
    #[serde(default)]
    pub started: bool,
    /// Its state as the engine last read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Why it did not end with its answer, as its machine reported it (`unknown` from one that
    /// predates causes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<crate::llm::implementation::Failure>,
    /// The time limit its session was launched with, in minutes; none before it was launched
    /// or in a record from before the limit was configurable (two hours).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_limit_minutes: Option<u32>,
    /// It was launched while its workflow's lead kept the backlog, so it got no work item
    /// writes and what it wrote anyway is detected (0.20.15).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lead_backlog: bool,
}

/// A child the run codes (for a task-category item, the item itself).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Child {
    pub id: u64,
    pub title: String,
    /// Its verified link.
    pub url: String,
    #[serde(default)]
    pub kind: String,
    /// The workflow repository it is coded in, and its checkout on the machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<crate::workflows::Repository>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    /// Its coding run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding: Option<Implementation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    /// The branch its session reported (`Branch: NAME` at the end of its report).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// A task's branches in the workflow's other repositories, as its session reported them
    /// (`Branch REPOSITORY: NAME`), each for a repository whose checkout the run was given.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<RepositoryBranch>,
    /// The repositories its report named a branch for that are not the workflow's (or have no
    /// checkout on its machine): those lines were ignored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_branches: Vec<String>,
    /// The pull requests of every branch it reported, each with its repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_requests: Vec<PullRequest>,
    /// `done`, or `accepted` when the user resumed the run past a coding run that did not end
    /// done (its record says how it ended) or past a missing pull request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Its predecessors as the engine last judged them before coding it (a planned child's).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<super::dependencies::Dependency>,
    /// The open pull request it was stacked on (`stack` policy): it starts from that branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<super::dependencies::Base>,
    /// The person resumed past retargeting its stacked pull request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retarget_accepted: bool,
    /// The review rounds of its pull requests, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rounds: Vec<Round>,
    /// Each reviewer's vote as last read, by `PULL_REQUEST:REVIEWER_ID`: a round starts when a
    /// vote turns negative, not while it stays so.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub votes: BTreeMap<String, i32>,
    /// The pages of the project's Wiki it delivered, once the person accepted them: a child
    /// delivered only so needs no pull request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wiki_pages: Vec<crate::ado::workflow::WikiPage>,
    /// How many times its lead continued its cut coding session, and the continuation waiting to
    /// be launched: the session it resumes and the lead's note (0.20.11).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub continuations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continue_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continue_note: Option<String>,
    /// The coding runs its continuations replaced, oldest first, and the deliveries they
    /// reported through their tools (a resumed turn has none of its own).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous: Vec<Implementation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_reports: Vec<Value>,
    /// What code found that its coding run delivered without reporting it (`found`): candidates
    /// for the person, never a delivery by themselves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found: Option<Found>,
}

/// What code read that a child's coding run delivered and did not report: the branches the
/// user pushed to the repositories the run was given since it started, the pull requests of
/// those branches and those linked to the work item, and the pages the user changed in the
/// project's Wiki. Each was read with the source's read credential and has its verified link;
/// none is claimed by another run.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Found {
    pub read_at: i64,
    /// The end of the window it was read in (the child's coding, plus a minute): fixed by the
    /// first read.
    pub until: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<FoundBranch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_requests: Vec<PullRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wiki_pages: Vec<crate::ado::workflow::WikiPage>,
    /// Some read failed, so what is listed may be incomplete.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

/// A branch found for a child: pushed by the user since its coding run started, or linked to
/// its work item.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct FoundBranch {
    pub repository: crate::workflows::Repository,
    pub branch: String,
    /// Its verified link.
    pub url: String,
    /// When the user last pushed it, when the pushes gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed_at: Option<String>,
    /// The work item links it (a branch or a pull request link).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub linked: bool,
}
impl Child {
    /// The review round in progress, if any.
    pub(in crate::app) fn open_round(&self) -> Option<&Round> {
        self.rounds.last().filter(|r| r.outcome.is_none())
    }
}

/// One review round of a child's pull request: who asked for changes, the threads open then,
/// the implementation run that corrected it (one more turn of the session that coded the child,
/// or a new session with a handover note), and how it ended.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Round {
    /// 1 for the pull request's first round.
    pub n: u32,
    /// `review` (a reviewer asked for changes) or `conflict` (the person asked the agent to
    /// update its branch after a conflict, `workflow_run_update_branch`).
    #[serde(default = "review_kind", skip_serializing_if = "is_review_kind")]
    pub kind: String,
    /// A conflict round's files, as the conflict listed them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    pub pull_request: u64,
    pub url: String,
    /// The repository's ID, in lowercase.
    pub repository: String,
    /// The branch it is corrected in.
    pub branch: String,
    /// The reviewers who voted against when it began (-10 rejected, -5 waiting for the author).
    pub reviewers: Vec<crate::ado::workflow::Reviewer>,
    /// The threads open then (active or pending, by someone other than its author), as data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<crate::ado::workflow::Thread>,
    /// The source branch's last commit when it began: a correction pushes another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    pub started_at: i64,
    /// The implementation run that corrects it, recorded before its machine is asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding: Option<Implementation>,
    /// `resumed` (one more turn of the child's session) or `new_session` (that session was
    /// gone or its machine cannot continue one: a new one with a handover note).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The stage it returns to once resubmitted.
    pub return_to: State,
    /// `resubmitted`, `disagreed`, `unverified`, `failed` or `accepted` (the person resumed past
    /// it); none while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    /// How each reviewer was asked again: `vote_reset` or `comment`, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested: Vec<String>,
    /// What code did not find after the correction (a reply, the summary comment, a push).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

fn review_kind() -> String {
    "review".into()
}
fn is_review_kind(kind: &String) -> bool {
    kind == "review"
}

/// A pull request of a run that conflicts with its target branch, as the engine found it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Conflict {
    pub pull_request: u64,
    pub url: String,
    /// The files it conflicts on (at most 100).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// The target commit the conflict was found against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_commit: Option<String>,
    /// Another workflow run's pull request completed into the same branch, the likely cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    pub at: i64,
}
impl Child {
    pub(in crate::app) fn accepted(&self) -> bool {
        self.outcome.as_deref() == Some("accepted")
    }
    /// Every branch it reported, with its repository: its own repository's first.
    pub(in crate::app) fn delivered(&self) -> Vec<(crate::workflows::Repository, String)> {
        let primary = self.repository.clone().zip(self.branch.clone()).into_iter();
        primary
            .chain(
                self.branches
                    .iter()
                    .map(|b| (b.repository.clone(), b.branch.clone())),
            )
            .collect()
    }
    /// Whether it reported any branch, or was given a delivery the person accepted (a branch or
    /// a Wiki page found without its report).
    pub(in crate::app) fn reported(&self) -> bool {
        self.branch.is_some() || !self.branches.is_empty() || !self.wiki_pages.is_empty()
    }
    /// Whether it delivered only Wiki pages: it waits for no pull request.
    pub(in crate::app) fn wiki_only(&self) -> bool {
        self.branch.is_none() && self.branches.is_empty() && !self.wiki_pages.is_empty()
    }
    /// The repositories of its reported branches that have no pull request yet.
    pub(in crate::app) fn without_pull_request(&self) -> Vec<String> {
        self.delivered()
            .into_iter()
            .filter(|(r, _)| {
                !self
                    .pull_requests
                    .iter()
                    .any(|p| p.repository.eq_ignore_ascii_case(&r.id))
            })
            .map(|(r, _)| r.name)
            .collect()
    }
}

/// A branch a task's coding session pushed in another of the workflow's repositories.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct RepositoryBranch {
    pub repository: crate::workflows::Repository,
    pub branch: String,
}

/// What an acting block did at one occurrence of its stage, or why it did not run. The engine
/// records the attempt (`attempting`) before it acts and the result after: an attempt found
/// without a result after a restart is `uncertain` and is never made again.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct BlockOutcome {
    pub block: String,
    pub kind: crate::workflows::Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<crate::workflows::Stage>,
    /// A notification: `sent`, `refused` or `uncertain`. A launcher: `succeeded`, `failed`,
    /// `timed_out`, `stopped`, `not_started` or `uncertain`. `attempting` until the result is recorded.
    pub outcome: String,
    /// When the result was recorded (the attempt's time while `attempting`).
    pub at: i64,
    /// Which occurrence of its stage this was (`done`, `child_done:ID`, ...): a block runs at
    /// most once for each.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub occurrence: String,
    /// Why a notification was refused or a launcher did not start (never a launcher's output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// A launcher's exit code, when the program ran to its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// A launcher's duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// A notification's Teams message ID, once it was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// One workflow run.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Run {
    pub id: String,
    /// The workflow's ID; none while classifying or unassigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// The workflow's definition when the run got it: later edits do not change this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<crate::workflows::Workflow>,
    /// Its blocks, in order, as they were then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<crate::workflows::Block>,
    pub item: Item,
    pub state: State,
    /// The stage it stopped in, while failed, interrupted or needing attention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_in: Option<State>,
    #[serde(default)]
    pub timeline: Vec<Stage>,
    /// What the director concluded, when it was asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning: Option<Implementation>,
    /// The creation ledger entry (the planning run's ID, as `implementation_create` keys it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Child>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_outcomes: Vec<BlockOutcome>,
    /// Progress summaries the director wrote, and the supervisor's drafts posted through it (`by:
    /// "supervisor"`) (`summary::Summary`: `id`, `text`, `created_at`,
    /// `provider`, `model`, `session_id`), oldest first, at most `summary::MAX_SUMMARIES`.
    /// A notification block's `{summary}` is rendered from the latest the director wrote.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summaries: Vec<Value>,
    /// One record per send of a summary to a chat (`summary::Send`), oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary_sends: Vec<Value>,
    /// What the user must look at, while the run waits on them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    /// The last thing that went wrong (kept after a resume).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The person resumed past an uncertain or incomplete creation: the children created are
    /// the ones coded.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub creation_accepted: bool,
    /// The person resumed a run that delivered no child: it ends done without pull requests.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub undelivered_accepted: bool,
    /// Why it stopped, when a resume treats that cause on its own (`nothing_delivered`,
    /// `ledger_parent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// While classifying: the director could not be reached; it is asked again from then on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<i64>,
    /// How many times in a row the director could not be reached (the wait doubles).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retries: u32,
    /// The watcher started it: the director chooses among the enabled workflows that start by
    /// themselves (`auto_start`) only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_start: bool,
    /// It holds one of the `concurrency` slots.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub admitted: bool,
    /// When the engine began to act on this run's acting blocks (0.12.8 or later). A run with
    /// none was recorded by an older version: the occurrences of its stages that had already
    /// happened are marked `skipped` when the engine first sees it, and never acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acting_since: Option<i64>,
    /// When the wait for its pull requests began (or began again on a resume).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_since: Option<i64>,
    /// Its lead was asked whether to keep waiting past the account's limit, and until when it
    /// said to (0.20.10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_asked: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_until: Option<i64>,
    /// What its lead wrote to its work items: each change once, with its outcome (`written`,
    /// `uncertain`, or why it was refused), never retried (0.20.12).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lead_writes: Vec<Value>,
    /// Its pull requests still unapproved past the wait, whose review support task its lead
    /// creates by the organization's norms, and those it created (each once: pull request,
    /// outcome, item), 0.20.14.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub support_due: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process_items: Vec<Value>,
    /// What the source's user wrote while its coding runs ran, that neither its lead nor Kuroko
    /// made (each once: key, item, link, `created` or `state`, state, time, child), 0.20.15.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runner_writes: Vec<Value>,
    /// When its pull requests were last read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    /// The run this one restarted (`workflow_run_restart`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<String>,
    /// The run that restarted this one; it was cancelled then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restarted_as: Option<String>,
    /// The user's handover note from the run this one restarted (at most
    /// [`MAX_HANDOVER`] characters): given to each of its coding runs, posted nowhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handover: Option<String>,
    /// The handover is the supervisor's draft, which the person did not edit: its coding runs
    /// get it as the supervisor's untrusted text, not as the person's note.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub handover_drafted: bool,
    /// The workflow supervisor restarted the run this one continues, as its workflow
    /// pre-approved: a pre-approved restart is never made of this run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preapproved_restart: bool,
    /// The workflow supervisor's looks at this run (`supervisor::Look`), oldest first, at most
    /// 20 (read only since the supervisor was retired).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supervision: Vec<super::supervisor::Look>,
    /// When it was moved ahead in the line for a slot (by the supervisor or the person); the
    /// mark is cleared when it takes a slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead_since: Option<i64>,
    /// The predecessors of its item as the engine last judged them (a task's are its child's).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<super::dependencies::Dependency>,
    /// While `waiting_dependency`: what it waits for and where it goes on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<super::dependencies::Wait>,
    /// The work item whose dependencies were judged last (the item, or a child).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies_for: Option<u64>,
    /// The work item whose dependencies were met (or skipped): its stage goes on without
    /// judging them again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies_met: Option<u64>,
    /// The predecessors the person skipped: they no longer count for this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_dependencies: Vec<u64>,
    /// The child whose wait for room among the open pull requests the person skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_skipped: Option<u64>,
    /// The repository (lowercase ID) its next child passed the workflow's limit of open pull
    /// requests for: it counts there until that child's coding run starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding_into: Option<String>,
    /// The conflict that stopped it last, while it needs attention for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<Conflict>,
    /// Conflicts the person resumed past, as `PULL_REQUEST:TARGET_COMMIT`: the same conflict
    /// against the same target commit does not stop the run again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts_accepted: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
}

impl Run {
    pub(in crate::app) fn new(item: Item, now: i64) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            workflow: None,
            definition: None,
            blocks: Vec::new(),
            item,
            state: State::Classifying,
            stopped_in: None,
            timeline: vec![Stage {
                stage: State::Classifying,
                started_at: now,
                finished_at: None,
                outcome: None,
            }],
            classification: None,
            planning: None,
            ledger: None,
            children: Vec::new(),
            block_outcomes: Vec::new(),
            summaries: Vec::new(),
            summary_sends: Vec::new(),
            attention: None,
            error: None,
            creation_accepted: false,
            undelivered_accepted: false,
            cause: None,
            retry_at: None,
            retries: 0,
            auto_start: false,
            admitted: false,
            acting_since: Some(now),
            waiting_since: None,
            wait_asked: None,
            wait_until: None,
            lead_writes: Vec::new(),
            support_due: Vec::new(),
            process_items: Vec::new(),
            runner_writes: Vec::new(),
            checked_at: None,
            continues: None,
            restarted_as: None,
            handover: None,
            handover_drafted: false,
            preapproved_restart: false,
            supervision: Vec::new(),
            ahead_since: None,
            dependencies: Vec::new(),
            wait: None,
            dependencies_for: None,
            dependencies_met: None,
            skipped_dependencies: Vec::new(),
            limit_skipped: None,
            coding_into: None,
            conflict: None,
            conflicts_accepted: Vec::new(),
            created_at: now,
            updated_at: now,
            finished_at: None,
        }
    }
    /// The dependencies judged last: those of the child being coded or waited for, else the
    /// item's.
    pub(in crate::app) fn subject_dependencies(&self) -> &[super::dependencies::Dependency] {
        match self.dependencies_for {
            Some(id) if id != self.item.id || self.task() => self
                .children
                .iter()
                .find(|c| c.id == id)
                .map_or(&self.dependencies[..], |c| &c.dependencies[..]),
            _ => &self.dependencies,
        }
    }
    /// A task-category item has no children: it is coded itself.
    pub(in crate::app) fn task(&self) -> bool {
        self.item.category == "task"
    }
    /// Close the current stage with `outcome`.
    fn close(&mut self, now: i64, outcome: &str) {
        if let Some(stage) = self.timeline.last_mut()
            && stage.finished_at.is_none()
        {
            stage.finished_at = Some(now);
            stage.outcome = Some(outcome.to_owned());
        }
    }
    /// Move to the next stage (or to `done`), closing the current one with `outcome`.
    pub(in crate::app) fn advance(&mut self, next: State, now: i64, outcome: &str) {
        self.close(now, outcome);
        // A slot is kept only from one stage that takes it to the next; any other run waits in
        // the line for one, in its place by creation.
        if !(self.state.takes_slot() && next.takes_slot()) {
            self.admitted = false;
        }
        self.state = next;
        self.stopped_in = None;
        self.attention = None;
        self.cause = None;
        self.retry_at = None;
        self.retries = 0;
        if next.driven() || next == State::Unassigned {
            self.timeline.push(Stage {
                stage: next,
                started_at: now,
                finished_at: None,
                outcome: None,
            });
        }
        if next == State::Done {
            self.timeline.push(Stage {
                stage: State::Done,
                started_at: now,
                finished_at: Some(now),
                outcome: Some("done".into()),
            });
            self.finished_at = Some(now);
        }
        self.updated_at = now;
    }
    /// Stop in `state` (failed, interrupted or needing attention) with what the user must read.
    pub(in crate::app) fn stop(&mut self, state: State, now: i64, reason: String) {
        if self.state.driven() {
            self.stopped_in = Some(self.state);
        }
        self.close(now, state.word());
        self.state = state;
        self.attention = Some(reason.clone());
        self.error = Some(reason);
        self.cause = None;
        self.admitted = false;
        self.updated_at = now;
    }
    /// Stop for good: cancelled.
    pub(in crate::app) fn cancel(&mut self, now: i64) {
        self.cancel_as(now, "cancelled");
    }
    /// Stop for good, closing the current stage with `outcome` (`cancelled`, `restarted`).
    pub(in crate::app) fn cancel_as(&mut self, now: i64, outcome: &str) {
        self.close(now, outcome);
        self.state = State::Cancelled;
        self.attention = None;
        self.admitted = false;
        self.finished_at = Some(now);
        self.updated_at = now;
    }
    /// The implementation run active now, if any: the planning run while planning, or the
    /// coding run of the child being coded.
    pub(in crate::app) fn active_implementation(&self) -> Option<&Implementation> {
        match self.stopped_in.unwrap_or(self.state) {
            State::Planning => self.planning.as_ref(),
            State::AddressingReview => self
                .children
                .iter()
                .find_map(|c| c.open_round())
                .and_then(|r| r.coding.as_ref()),
            State::Coding => self
                .children
                .iter()
                .find(|c| c.outcome.is_none())
                .and_then(|c| c.coding.as_ref()),
            _ => None,
        }
    }
    /// Every implementation run it asked for: the planning run and each child's coding run.
    pub(in crate::app) fn implementations(&self) -> impl Iterator<Item = &Implementation> {
        self.planning
            .iter()
            .chain(self.children.iter().flat_map(|c| {
                c.previous
                    .iter()
                    .chain(c.coding.iter())
                    .chain(c.rounds.iter().filter_map(|r| r.coding.as_ref()))
            }))
    }
    /// It waits in the line for a slot: in a stage that takes one, without one.
    pub(in crate::app) fn waits_for_slot(&self) -> bool {
        self.state.takes_slot() && !self.admitted
    }
    /// The run as the operations show it: the whole record, with the stage it is in.
    pub(in crate::app) fn view(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        value["stage"] = json!(self.stopped_in.unwrap_or(self.state).word());
        value["waits_on_user"] = json!(self.state.waits_on_user());
        // What its coding runs may do beyond the fixed rules, as the definition it got holds it.
        value["allow"] = json!(self.definition.as_ref().map(|d| &d.allow));
        value
    }
    /// The run in a list: its identity, item, state, stage and what it waits for.
    pub(in crate::app) fn summary(&self) -> Value {
        json!({
            "id": self.id,
            "workflow": self.workflow,
            "item": {"id": self.item.id, "title": self.item.title, "url": self.item.url,
                "kind": self.item.kind, "category": self.item.category, "project": self.item.project},
            "state": self.state,
            "stage": self.stopped_in.unwrap_or(self.state).word(),
            "attention": self.attention,
            "waits_on_user": self.state.waits_on_user(),
            "children": self.children.len(),
            "pull_requests": self.children.iter().flat_map(|c| c.pull_requests.iter())
                .map(|p| json!({"url": p.url, "status": p.status})).collect::<Vec<_>>(),
            "continues": self.continues,
            "restarted_as": self.restarted_as,
            "supervision": self.supervision.last().map(|l| json!({
                "look": l.id, "event": l.event, "outcome": l.outcome, "created_at": l.created_at,
                "proposed": self.supervision.iter().flat_map(|l| &l.actions).filter(|a| a.state == "proposed").count(),
                "not_verified": l.checks.iter().filter(|c| c.result == "not_verified").count(),
            })),
            "ahead": self.ahead_since.is_some(),
            "waiting_for": self.wait.as_ref().map(|w| json!({"subject": w.subject, "text": w.text, "since": w.since})),
            "dependencies": self.subject_dependencies().iter().map(|d| json!({"id": d.id, "url": d.url, "satisfied": d.satisfied})).collect::<Vec<_>>(),
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "finished_at": self.finished_at,
        })
    }
}

/// The longest handover note, in characters.
pub(in crate::app) const MAX_HANDOVER: usize = 4000;

/// What a pause does to the agent sessions of the runs it covers when it begins.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(in crate::app) enum Sessions {
    /// Stop every live session of those runs at once; each run with one stops `interrupted`.
    Stop,
    /// Leave the live sessions to finish; the engine still observes them.
    Finish,
}

/// A pause of the workflow runs: of every project, or of one project (by name, in any
/// organization). While one covers a run, the engine starts nothing for it: no slot, no
/// director session, no planning or coding run and no acting block; it still observes the
/// implementation runs already started and reads the creation ledger and pull requests. The
/// watcher starts no run for a paused project. A paused run keeps its place by creation in the
/// line for a slot, and takes it again when the pause ends.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Pause {
    /// The project's name; none for every project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub sessions: Sessions,
    pub paused_at: i64,
}
impl Pause {
    pub(in crate::app) fn covers(&self, project: &str) -> bool {
        self.project
            .as_deref()
            .is_none_or(|p| p.eq_ignore_ascii_case(project))
    }
}
/// Whether a pause covers a run's project.
pub(in crate::app) fn paused(pauses: &[Pause], project: &str) -> bool {
    pauses.iter().any(|p| p.covers(project))
}

/// The line for the `concurrency` slots, from `runs` oldest first (`Store::list` reversed):
/// every run waiting for a slot, in creation order, and in its place a classifying run that is
/// not waiting out a retry, which will need one once classified. A run that gave a slot back
/// keeps its place, so a run created later never passes it.
/// A paused run is out of the line while its pause lasts, so it holds up no run of a project
/// that is not paused, and back in its place when the pause ends.
/// A run moved ahead (`ahead_since`) goes before every run that was not, the runs moved ahead
/// in the order they were moved; every other run keeps its place by creation.
pub(in crate::app) fn line<'a>(runs: &'a [Run], now: i64, pauses: &[Pause]) -> Vec<&'a Run> {
    let mut line: Vec<&Run> = runs
        .iter()
        .filter(|r| {
            r.waits_for_slot()
                || (r.state == State::Classifying && r.retry_at.is_none_or(|at| at <= now))
        })
        .filter(|r| !paused(pauses, &r.item.project))
        // A run whose next launch needs directories another run's agent holds on that machine
        // waits out of the line, so it takes no slot it could not use and holds up no run of
        // another machine or checkout; it is back in its place by creation once they are free.
        .filter(|r| held_by(runs, r).is_none())
        .collect();
    // Stable: equal keys keep their order by creation.
    line.sort_by_key(|r| (r.ahead_since.is_none(), r.ahead_since.unwrap_or_default()));
    line
}

/// The machine and directories the implementation run `run` has launched holds now: its
/// planning run (every checkout of its workflow), or the coding run of the child being coded
/// (a task's run is given every checkout, a planned child its own), while its state as last
/// observed is not an ending.
pub(in crate::app) fn holds(run: &Run) -> Option<(&str, Vec<PathBuf>)> {
    if run.state.finished() {
        return None;
    }
    let active = run.active_implementation()?;
    let running = active
        .state
        .as_deref()
        .is_none_or(|s| matches!(s, "running" | "planning"));
    if !active.started || !running {
        return None;
    }
    Some((active.machine.as_str(), directories(run)))
}

/// The machine and directories the next launch of `run` will need: a planning run not yet
/// launched, or the coding run of the next child not yet launched. A planned child's checkout
/// is chosen when it is launched, so until then it counts as needing every checkout.
pub(in crate::app) fn needs(run: &Run) -> Option<(&str, Vec<PathBuf>)> {
    let definition = run.definition.as_ref()?;
    let pending = match run.state {
        State::Planning => run.planning.as_ref().is_none_or(|p| !p.started),
        State::Coding => run
            .children
            .iter()
            .find(|c| c.outcome.is_none())
            .is_some_and(|c| c.coding.as_ref().is_none_or(|i| !i.started)),
        State::AddressingReview => run
            .children
            .iter()
            .find_map(|c| c.open_round())
            .is_some_and(|r| r.coding.as_ref().is_none_or(|i| !i.started)),
        _ => false,
    };
    pending.then(|| (definition.machine.as_str(), directories(run)))
}

/// The directories a run's agent works in: its child's checkout when the child being coded is a
/// planned one already placed, otherwise every checkout of its workflow.
fn directories(run: &Run) -> Vec<PathBuf> {
    let stage = run.stopped_in.unwrap_or(run.state);
    let child = match stage {
        State::Coding if !run.task() => run.children.iter().find(|c| c.outcome.is_none()),
        State::AddressingReview if !run.task() => {
            run.children.iter().find(|c| c.open_round().is_some())
        }
        _ => None,
    }
    .and_then(|c| c.directory.clone());
    match child {
        Some(directory) => vec![directory],
        None => run
            .definition
            .iter()
            .flat_map(|d| d.checkouts.values().cloned())
            .collect(),
    }
}

/// Whether two directories, as their machine spells them, are one inside the other (the
/// machine's own check), whatever system reads them: separators and, for a Windows path,
/// letter case are compared alike.
fn overlap(a: &Path, b: &Path) -> bool {
    let spell = |p: &Path| {
        let text = p.to_string_lossy().replace('\\', "/");
        let text = if text.contains(':') || text.starts_with("//") {
            text.to_lowercase()
        } else {
            text
        };
        format!("{}/", text.trim_end_matches('/'))
    };
    let (a, b) = (spell(a), spell(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// The run whose launched agent holds, on the machine `run` will launch on, a directory its next
/// launch needs; none when it needs nothing or nothing it needs is held.
pub(in crate::app) fn held_by<'a>(runs: &'a [Run], run: &Run) -> Option<&'a Run> {
    let (machine, wanted) = needs(run)?;
    runs.iter().filter(|r| r.id != run.id).find(|other| {
        holds(other).is_some_and(|(at, held)| {
            at == machine && held.iter().any(|h| wanted.iter().any(|w| overlap(h, w)))
        })
    })
}

/// Whether run `id` takes a slot now: fewer runs are ahead of it in the line than slots are
/// free.
pub(in crate::app) fn may_take_slot(
    runs: &[Run],
    id: &str,
    concurrency: usize,
    now: i64,
    pauses: &[Pause],
) -> bool {
    let taken = runs
        .iter()
        .filter(|r| r.admitted && r.state.takes_slot() && r.id != id)
        .count();
    let free = concurrency.saturating_sub(taken);
    line(runs, now, pauses)
        .iter()
        .position(|r| r.id == id)
        .is_some_and(|ahead| ahead < free)
}

/// The place of each run waiting for a slot (1 is next), by run ID; a paused run has none.
pub(in crate::app) fn positions(
    runs: &[Run],
    now: i64,
    pauses: &[Pause],
) -> HashMap<String, usize> {
    line(runs, now, pauses)
        .into_iter()
        .enumerate()
        .filter(|(_, r)| r.waits_for_slot())
        .map(|(at, r)| (r.id.clone(), at + 1))
        .collect()
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

pub(in crate::app) struct Store(std::sync::Mutex<Connection>);

impl Store {
    pub(in crate::app) fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        security::protect_file(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS workflow_runs (
                id TEXT PRIMARY KEY, workflow_id TEXT NOT NULL, item_provider TEXT NOT NULL,
                item_id INTEGER NOT NULL, item_url TEXT NOT NULL, state TEXT NOT NULL,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, payload TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS workflow_runs_item ON workflow_runs(item_url, created_at);
            CREATE INDEX IF NOT EXISTS workflow_runs_workflow ON workflow_runs(workflow_id, created_at);
            CREATE INDEX IF NOT EXISTS workflow_runs_state ON workflow_runs(state, updated_at);
            CREATE UNIQUE INDEX IF NOT EXISTS workflow_runs_active_item ON workflow_runs(item_url)
                WHERE state NOT IN ('done','cancelled');
            CREATE TABLE IF NOT EXISTS workflow_pauses (
                scope TEXT PRIMARY KEY, payload TEXT NOT NULL);",
        )?;
        Ok(Self(std::sync::Mutex::new(db)))
    }
    /// Record a new run; refused while the item has an active run.
    pub(in crate::app) fn insert(&self, run: &Run) -> Result<bool> {
        let db = self.0.lock().unwrap();
        let inserted = db.execute(
            "INSERT OR IGNORE INTO workflow_runs (id,workflow_id,item_provider,item_id,item_url,state,created_at,updated_at,payload) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                run.id,
                run.workflow.clone().unwrap_or_default(),
                run.item.provider,
                run.item.id,
                run.item.url,
                run.state.word(),
                run.created_at,
                run.updated_at,
                serde_json::to_string(run)?
            ],
        )?;
        Ok(inserted == 1)
    }
    pub(in crate::app) fn save(&self, run: &Run) -> Result<()> {
        let db = self.0.lock().unwrap();
        let changed = db.execute(
            "UPDATE workflow_runs SET workflow_id=?2,state=?3,updated_at=?4,payload=?5 WHERE id=?1",
            params![
                run.id,
                run.workflow.clone().unwrap_or_default(),
                run.state.word(),
                run.updated_at,
                serde_json::to_string(run)?
            ],
        )?;
        ensure!(changed == 1, "Workflow run {} is not recorded.", run.id);
        Ok(())
    }
    /// The pauses in force, every project's first.
    pub(in crate::app) fn pauses(&self) -> Result<Vec<Pause>> {
        let db = self.0.lock().unwrap();
        let mut query = db.prepare("SELECT payload FROM workflow_pauses ORDER BY scope")?;
        let rows = query.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    /// Record a pause, replacing the one of the same scope.
    pub(in crate::app) fn pause(&self, pause: &Pause) -> Result<()> {
        let scope = pause.project.as_deref().unwrap_or_default().to_lowercase();
        self.0.lock().unwrap().execute(
            "INSERT OR REPLACE INTO workflow_pauses (scope, payload) VALUES (?1, ?2)",
            params![scope, serde_json::to_string(pause)?],
        )?;
        Ok(())
    }
    /// End the pause of one project, or every pause; returns how many ended.
    pub(in crate::app) fn unpause(&self, project: Option<&str>) -> Result<usize> {
        let db = self.0.lock().unwrap();
        Ok(match project {
            Some(project) => db.execute(
                "DELETE FROM workflow_pauses WHERE scope=?1",
                [project.to_lowercase()],
            )?,
            None => db.execute("DELETE FROM workflow_pauses", [])?,
        })
    }
    /// Lose a run, as a failing disk would: a test of what is reported when saving fails.
    #[cfg(all(test, unix))]
    pub(in crate::app) fn forget(&self, id: &str) {
        let db = self.0.lock().unwrap();
        db.execute("DELETE FROM workflow_runs WHERE id=?1", [id])
            .unwrap();
    }
    pub(in crate::app) fn get(&self, id: &str) -> Result<Option<Run>> {
        let db = self.0.lock().unwrap();
        let payload: Option<String> = db
            .query_row("SELECT payload FROM workflow_runs WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        payload
            .map(|p| serde_json::from_str(&p).map_err(Into::into))
            .transpose()
    }
    /// The active run of an item (any state but done and cancelled).
    pub(in crate::app) fn active_for(&self, url: &str) -> Result<Option<Run>> {
        let db = self.0.lock().unwrap();
        let payload: Option<String> = db
            .query_row(
                "SELECT payload FROM workflow_runs WHERE item_url=?1 AND state NOT IN ('done','cancelled')",
                [url],
                |r| r.get(0),
            )
            .optional()?;
        payload
            .map(|p| serde_json::from_str(&p).map_err(Into::into))
            .transpose()
    }
    /// Runs, the most recently created first: in one of `states` (all when empty), of one
    /// workflow, at most `limit` (-1 for all).
    pub(in crate::app) fn list(
        &self,
        states: &[State],
        workflow: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Run>> {
        let db = self.0.lock().unwrap();
        let words: Vec<&str> = states.iter().map(|s| s.word()).collect();
        let filter = serde_json::to_string(&words)?;
        let mut query = db.prepare(
            "SELECT id, payload FROM workflow_runs WHERE (?1 = '[]' OR state IN (SELECT value FROM json_each(?1))) AND (?2 IS NULL OR workflow_id=?2) ORDER BY created_at DESC, rowid DESC LIMIT ?3",
        )?;
        let rows = query.query_map(params![filter, workflow, limit], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut runs = Vec::new();
        for row in rows {
            let (id, payload) = row?;
            // A record this version cannot read (written by a newer one, before a rollback) is
            // left out rather than failing every list: the engine keeps driving the others.
            match serde_json::from_str(&payload) {
                Ok(run) => runs.push(run),
                Err(_) => tracing::warn!(event = "workflow_run_unreadable", run = %id),
            }
        }
        Ok(runs)
    }
}
