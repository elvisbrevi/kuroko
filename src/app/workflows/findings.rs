//! Findings: what a run's coding session found that is not the requested change but that
//! someone must know or decide, reported through its run's tools (`finding_report`) and kept
//! here once per report, in `workflow_findings` in `workflows.db`. Nothing here writes outside
//! this record: a finding is listed, resolved or dismissed by the person.
use super::record::Run;
use super::runs::require_engine;
use super::*;
use rusqlite::{Connection, params};

/// One finding as it is kept and listed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Finding {
    pub id: String,
    pub workflow: String,
    pub run: String,
    /// The child's work item (the run's item for a task) and its verified link.
    pub item: u64,
    pub item_url: String,
    /// The coding run that reported it and the report's number.
    pub implementation_run: String,
    pub seq: u32,
    pub title: String,
    pub detail: String,
    /// `blocking`, `important` or `minor`.
    pub severity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub evidence: String,
    /// `runner` (later also `lead` and `comment`).
    pub source: String,
    /// `open`, `resolved` or `dismissed`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The findings of `workflows.db` (its own connection beside the runs').
pub(in crate::app) struct Findings(std::sync::Mutex<Connection>);
impl Findings {
    pub(in crate::app) fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        security::protect_file(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_findings (
                id TEXT PRIMARY KEY, key TEXT NOT NULL UNIQUE, workflow_id TEXT NOT NULL,
                run_id TEXT NOT NULL, state TEXT NOT NULL, created_at INTEGER NOT NULL,
                payload TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS workflow_findings_workflow ON workflow_findings(workflow_id, created_at);
            CREATE INDEX IF NOT EXISTS workflow_findings_run ON workflow_findings(run_id, created_at);",
        )?;
        Ok(Self(std::sync::Mutex::new(db)))
    }
    /// Keep a finding once per report (`implementation run:seq`).
    pub(in crate::app) fn insert(&self, finding: &Finding) -> Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT OR IGNORE INTO workflow_findings (id,key,workflow_id,run_id,state,created_at,payload) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                finding.id,
                format!("{}:{}", finding.implementation_run, finding.seq),
                finding.workflow,
                finding.run,
                finding.state,
                finding.created_at,
                serde_json::to_string(finding)?
            ],
        )?;
        Ok(())
    }
    /// The highest report number kept for `implementation_run` (a lead's findings after a cut
    /// turn whose shift was not saved).
    pub(in crate::app) fn last_seq(&self, implementation_run: &str) -> Result<u32> {
        let db = self.0.lock().unwrap();
        let mut query = db.prepare("SELECT key FROM workflow_findings WHERE key LIKE ?1")?;
        let prefix = format!("{implementation_run}:");
        let keys = query.query_map(params![format!("{prefix}%")], |r| r.get::<_, String>(0))?;
        Ok(keys
            .filter_map(|k| k.ok()?.strip_prefix(&prefix)?.parse::<u32>().ok())
            .max()
            .unwrap_or(0))
    }
    pub(in crate::app) fn list(
        &self,
        workflow: Option<&str>,
        run: Option<&str>,
        state: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Finding>> {
        let db = self.0.lock().unwrap();
        let mut query = db.prepare(
            "SELECT payload FROM workflow_findings WHERE (?1 IS NULL OR workflow_id=?1) AND (?2 IS NULL OR run_id=?2) AND (?3 IS NULL OR state=?3) ORDER BY created_at DESC, rowid DESC LIMIT ?4",
        )?;
        let rows = query.query_map(params![workflow, run, state, limit], |r| {
            r.get::<_, String>(0)
        })?;
        let mut found = Vec::new();
        for row in rows {
            // A row this version cannot read (a newer one's) is left out of the list.
            if let Ok(finding) = serde_json::from_str(&row?) {
                found.push(finding);
            }
        }
        Ok(found)
    }
    fn get(&self, id: &str) -> Result<Option<Finding>> {
        let db = self.0.lock().unwrap();
        let payload: Option<String> = rusqlite::OptionalExtension::optional(db.query_row(
            "SELECT payload FROM workflow_findings WHERE id=?1",
            [id],
            |r| r.get(0),
        ))?;
        Ok(payload.and_then(|p| serde_json::from_str(&p).ok()))
    }
    fn save(&self, finding: &Finding) -> Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE workflow_findings SET state=?2, payload=?3 WHERE id=?1",
            params![finding.id, finding.state, serde_json::to_string(finding)?],
        )?;
        Ok(())
    }
}

/// Keep the findings an observed coding run of child `index` reported (`observed["reports"]`),
/// each once.
pub(in crate::app) fn record(state: &DesktopState, run: &Run, index: usize, observed: &Value) {
    let (Some(child), Some(coding)) = (
        run.children.get(index),
        run.children.get(index).and_then(|c| c.coding.as_ref()),
    ) else {
        return;
    };
    for report in observed["reports"].as_array().into_iter().flatten() {
        if report["kind"] != "finding" {
            continue;
        }
        let at = report["at"]
            .as_str()
            .and_then(|a| chrono::DateTime::parse_from_rfc3339(a).ok())
            .map_or_else(|| chrono::Utc::now().timestamp(), |a| a.timestamp());
        let text = |key: &str| report[key].as_str().unwrap_or_default().to_owned();
        let finding = Finding {
            id: uuid::Uuid::new_v4().to_string(),
            workflow: run.workflow.clone().unwrap_or_default(),
            run: run.id.clone(),
            item: child.id,
            item_url: child.url.clone(),
            implementation_run: coding.id.clone(),
            seq: report["seq"]
                .as_u64()
                .and_then(|s| u32::try_from(s).ok())
                .unwrap_or(0),
            title: text("title"),
            detail: text("detail"),
            severity: text("severity"),
            evidence: text("evidence"),
            source: "runner".into(),
            state: "open".into(),
            note: None,
            created_at: at,
            updated_at: at,
        };
        if let Err(error) = state.workflows.findings.insert(&finding) {
            tracing::warn!(event = "workflow_finding_unrecorded", run = %run.id, error = %error);
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    workflow: Option<String>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettleArgs {
    finding: String,
    #[serde(default)]
    note: Option<String>,
}

pub(in crate::app) fn operation(
    state: &DesktopState,
    method: &str,
    args: &Value,
) -> Result<Value, String> {
    require_engine(state)?;
    match method {
        "workflow_findings" => {
            let a: ListArgs = parse(args)?;
            if let Some(word) = a.state.as_deref()
                && !matches!(word, "open" | "resolved" | "dismissed")
            {
                return Err(invalid("A finding's state is open, resolved or dismissed."));
            }
            let limit = a.limit.unwrap_or(50).clamp(1, 200);
            let findings = state
                .workflows
                .findings
                .list(
                    a.workflow.as_deref(),
                    a.run.as_deref(),
                    a.state.as_deref(),
                    limit,
                )
                .map_err(fail)?;
            Ok(json!({ "findings": findings }))
        }
        "workflow_finding_resolve" | "workflow_finding_dismiss" => {
            let a: SettleArgs = parse(args)?;
            let mut finding = state
                .workflows
                .findings
                .get(&a.finding)
                .map_err(fail)?
                .ok_or_else(|| invalid(format!("Finding {} does not exist.", a.finding)))?;
            if finding.state != "open" {
                return Err(conflict(format!(
                    "Finding {} is already {}.",
                    finding.id, finding.state
                )));
            }
            let note: Option<String> = a
                .note
                .map(|n| {
                    n.chars()
                        .filter(|c| !c.is_control())
                        .take(1000)
                        .collect::<String>()
                })
                .map(|n| n.trim().to_owned())
                .filter(|n| !n.is_empty());
            finding.state = if method == "workflow_finding_resolve" {
                "resolved"
            } else {
                "dismissed"
            }
            .into();
            finding.note = note;
            finding.updated_at = chrono::Utc::now().timestamp();
            state.workflows.findings.save(&finding).map_err(fail)?;
            Ok(json!({ "finding": finding }))
        }
        _ => Err(invalid("Unknown workflow operation.")),
    }
}
