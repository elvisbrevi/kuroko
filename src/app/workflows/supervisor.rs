//! The retired workflow supervisor (0.20.16): a workflow's lead follows its runs now
//! (`shifts`). What the supervisor looked at and audited stays readable
//! (`workflow_supervision`: a run's looks, as stored on it, and the audits in
//! `workflow_audits`), and the person's control of the line for slots stays
//! (`workflow_run_requeue`); asking for a look or an audit, carrying out or dismissing what it
//! proposed answer that the lead replaced it. Nothing here runs a model or reads a provider.
use super::record::{Run, State as Phase};
use super::runs::{found, require_engine};
use super::*;
use crate::llm::implementation::Failure;
use crate::workflows::director::Director;
#[cfg(all(test, unix))]
use rusqlite::params;
use rusqlite::{Connection, OptionalExtension};

pub(in crate::app) mod audit;

pub(in crate::app) const METHODS: [&str; 5] = [
    "workflow_supervise",
    "workflow_supervision",
    "workflow_supervision_act",
    "workflow_supervision_dismiss",
    "workflow_run_requeue",
];

/// What the retired operations answer.
pub(in crate::app) const RETIRED: &str = "The workflow supervisor was retired: a workflow's lead follows its runs now (workflow_lead_shifts, workflow_lead_wake, workflow_lead_connect). Its earlier looks and audits are still read with workflow_supervision.";

/// What the supervisor said about a run.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Assessment {
    pub done: String,
    pub left: String,
    pub recommendation: String,
}

/// One claim of a final report, as code checked it against what it read.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Check {
    /// The claim, cleaned and cut.
    pub claim: String,
    /// `pull_request`, `branch`, `item_state` or `other`.
    pub kind: String,
    /// What code parsed from it: a pull request number, a branch name or a state; empty when
    /// nothing could be parsed.
    #[serde(default, rename = "ref", skip_serializing_if = "String::is_empty")]
    pub reference: String,
    /// `verified` or `not_verified`.
    pub result: String,
    /// Code's sentence about what it read.
    pub fact: String,
    /// `report` when code found the claim in a final report by itself, `supervisor` when the
    /// supervisor listed it.
    pub from: String,
}

/// A work item the supervisor drafted, for the person to create with Create request.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Draft {
    /// `requirement` or `bug`.
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub description: String,
    /// The workflow repository it is for, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<crate::workflows::Repository>,
    pub reason: String,
}

/// One proposed action and what became of it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Action {
    /// Its place in the look (`a1`…).
    pub id: String,
    /// One of `SUPERVISOR_ACTIONS`.
    pub action: String,
    /// `stop` or `finish`, for `pause_project`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sessions: String,
    /// The index of its drafted work item, for `create_work_item`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item: Option<usize>,
    pub reason: String,
    /// `proposed`; `done` for what the supervisor does by itself (`move_ahead`); `attempting`
    /// while the person's confirmed action runs, then `executed`, `refused` or `uncertain`;
    /// `opened` for a work item handed to Create request; `expired` when the run moved on
    /// since the look; `dismissed`.
    pub state: String,
    /// Who acted: `supervisor` (by itself) or `person`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    /// Why it was refused or is uncertain (a fixed text of this package).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// One look of the supervisor at a run, as stored on it (`Run::supervision`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Look {
    /// Its session's record in Agent history.
    pub id: String,
    /// The event that started it: `run_stopped`, `run_waiting`, `machine_offline`,
    /// `session_disagrees`, or `requested`.
    pub event: String,
    /// The event keys it covered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    /// Events of this run skipped (over the daily cap) since its previous look.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missed: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requested: bool,
    pub created_at: i64,
    pub supervised_by: Director,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// `done` or `failed`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    /// What code could not read before the session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<String>,
    /// What code cut to keep the input within its bound.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncated: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<Assessment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
    /// The drafted handover note for a restart.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handover: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work_items: Vec<Draft>,
    /// The drafted progress summary, with the links code builds.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Action>,
    /// Actions and drafts code dropped (another run, outside the list, a repository that is
    /// not the workflow's).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u32,
    /// The run as it was when looked at: an action whose run moved on is expired.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub marker: String,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The supervisor's audits in `workflows.db` (its own connection beside the runs'), kept to be
/// read.
pub(in crate::app) struct Looks(std::sync::Mutex<Connection>);
impl Looks {
    pub(in crate::app) fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        security::protect_file(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_audits (
                id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, payload TEXT NOT NULL);",
        )?;
        Ok(Self(std::sync::Mutex::new(db)))
    }
    /// Store an audit (an earlier version's, in tests).
    #[cfg(all(test, unix))]
    pub(in crate::app) fn insert_audit(&self, audit: &audit::Audit) -> Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT OR REPLACE INTO workflow_audits (id, created_at, payload) VALUES (?1, ?2, ?3)",
            params![audit.id, audit.created_at, serde_json::to_string(audit)?],
        )?;
        Ok(())
    }
    /// The most recent audits, newest first.
    pub(in crate::app) fn audits(&self, limit: usize) -> Result<Vec<audit::Audit>> {
        let db = self.0.lock().unwrap();
        let mut query = db.prepare(
            "SELECT payload FROM workflow_audits ORDER BY created_at DESC, rowid DESC LIMIT ?1",
        )?;
        let rows = query.query_map([limit as i64], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    /// One audit by its ID.
    pub(in crate::app) fn audit(&self, id: &str) -> Result<Option<audit::Audit>> {
        let db = self.0.lock().unwrap();
        let payload: Option<String> = db
            .query_row(
                "SELECT payload FROM workflow_audits WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        payload
            .map(|p| serde_json::from_str(&p).map_err(Into::into))
            .transpose()
    }
}

/// What is left of the supervisor on the profile: its audits.
pub(in crate::app) struct Supervision {
    pub(in crate::app) looks: Looks,
}
impl Supervision {
    pub(in crate::app) fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            looks: Looks::open(path)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupervisionArgs {
    /// A run's looks.
    #[serde(default)]
    run: Option<String>,
    /// One audit, by ID or `latest`.
    #[serde(default)]
    audit: Option<String>,
    /// The list of audits.
    #[serde(default)]
    audits: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequeueArgs {
    run: String,
    ahead: bool,
}

pub(super) async fn operation(
    host: &Arc<Host>,
    method: &str,
    args: &Value,
) -> Result<Value, String> {
    match method {
        "workflow_supervision" => supervision(host, parse(args)?),
        "workflow_run_requeue" => requeue(host, parse(args)?).await,
        "workflow_supervise" | "workflow_supervision_act" | "workflow_supervision_dismiss" => {
            Err(conflict(RETIRED))
        }
        _ => Err(invalid("Unknown workflow operation.")),
    }
}

/// `workflow_supervision {run | audit | audits}`: what the supervisor looked at and audited.
fn supervision(host: &Arc<Host>, args: SupervisionArgs) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let looks = &state.workflows.supervision.looks;
    match (&args.run, &args.audit, args.audits) {
        (None, Some(id), false) => {
            let found = if id == "latest" {
                looks.audits(1).map_err(fail)?.into_iter().next()
            } else {
                looks.audit(id).map_err(fail)?
            };
            Ok(json!({"audit": found, "retired": RETIRED}))
        }
        (None, None, true) => {
            let audits: Vec<Value> = looks
                .audits(audit::MAX_AUDITS)
                .map_err(fail)?
                .iter()
                .map(audit::Audit::summary)
                .collect();
            Ok(json!({"audits": audits, "retired": RETIRED}))
        }
        (Some(run), None, false) => {
            let mut run = found(state, run)?;
            // An action an older host left being carried out may or may not have been.
            for action in run.supervision.iter_mut().flat_map(|l| &mut l.actions) {
                if action.state == "attempting" {
                    action.state = "uncertain".into();
                }
            }
            Ok(json!({"run": run.id, "looks": run.supervision, "retired": RETIRED}))
        }
        _ => Err(invalid(
            "Give a run, an audit (an ID or latest), or audits: true.",
        )),
    }
}

/// Whether a run waits in the line for a slot (or will, once classified).
fn waits_in_line(run: &Run) -> bool {
    run.waits_for_slot() || run.state == Phase::Classifying
}

/// `workflow_run_requeue {run, ahead}`: move a run that waits in the line ahead, or put it
/// back in its place by creation.
async fn requeue(host: &Arc<Host>, args: RequeueArgs) -> Result<Value, String> {
    let state = &host.state;
    require_engine(state)?;
    let lock = state.workflows.lock(&args.run);
    let _guard = lock.lock().await;
    let mut run = found(state, &args.run)?;
    if !waits_in_line(&run) {
        return Err(conflict(format!(
            "Workflow run {} does not wait for a slot ({}).",
            run.id,
            run.state.word()
        )));
    }
    if args.ahead {
        run.ahead_since.get_or_insert(now());
    } else {
        run.ahead_since = None;
    }
    state.workflows.db.save(&run).map_err(fail)?;
    Ok(json!({"run": run.id, "ahead": run.ahead_since.is_some(), "ahead_since": run.ahead_since}))
}

// The tests use the engine's bench, whose fake provider CLIs are shell scripts.
#[cfg(all(test, unix))]
mod tests;
