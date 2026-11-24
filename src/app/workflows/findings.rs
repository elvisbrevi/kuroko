//! Findings: what a run's coding session found that is not the requested change but that
//! someone must know or decide, reported through its run's tools (`finding_report`) and kept
//! here once per report, in `workflow_findings` in `workflows.db`. Nothing here writes outside
//! this record: a finding is listed, resolved or dismissed by the person.
use super::engine;
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
    /// The work item its workflow's lead drafted for it (`proposed`), as a child of the run's
    /// item: type, title, description and fields (0.20.13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<Value>,
    /// The work item created from that draft, once: its ID and verified link, who approved it
    /// (`person`, or `lead` for a blocking finding its workflow lets it create) and when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<Value>,
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
        let findings = Self(std::sync::Mutex::new(db));
        // A creation the host stopped in the middle of may or may not have reached Azure DevOps.
        for mut finding in findings.list(None, None, Some("sending"), -1)? {
            finding.state = "uncertain".into();
            findings.save(&finding)?;
        }
        Ok(findings)
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
    /// Keep a finding unless one with its key is kept already; whether it was kept.
    pub(in crate::app) fn insert_new(&self, finding: &Finding) -> Result<bool> {
        let before = self.0.lock().unwrap().total_changes();
        self.insert(finding)?;
        Ok(self.0.lock().unwrap().total_changes() > before)
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
    /// The open findings whose key starts with `prefix` (what code detected of one kind),
    /// newest first.
    pub(in crate::app) fn open_with_prefix(
        &self,
        prefix: &str,
        limit: i64,
    ) -> Result<Vec<Finding>> {
        let db = self.0.lock().unwrap();
        let mut query = db.prepare(
            "SELECT payload FROM workflow_findings WHERE state='open' AND substr(key, 1, length(?1))=?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = query.query_map(params![prefix, limit], |r| r.get::<_, String>(0))?;
        Ok(rows
            .filter_map(|row| serde_json::from_str(&row.ok()?).ok())
            .collect())
    }
    pub(in crate::app) fn get(&self, id: &str) -> Result<Option<Finding>> {
        let db = self.0.lock().unwrap();
        let payload: Option<String> = rusqlite::OptionalExtension::optional(db.query_row(
            "SELECT payload FROM workflow_findings WHERE id=?1",
            [id],
            |r| r.get(0),
        ))?;
        Ok(payload.and_then(|p| serde_json::from_str(&p).ok()))
    }
    pub(in crate::app) fn save(&self, finding: &Finding) -> Result<()> {
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
            draft: None,
            created: None,
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
                && !matches!(
                    word,
                    "open"
                        | "resolved"
                        | "dismissed"
                        | "proposed"
                        | "sending"
                        | "item_created"
                        | "uncertain"
                )
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
            // An open finding, a draft the person turns down, or one whose uncertain item they
            // checked by hand; never while its item is being created.
            let creating = state.workflows.lock("finding-create");
            let Ok(_one) = creating.try_lock() else {
                return Err(conflict(
                    "A finding's work item is being created: try again in a moment.",
                ));
            };
            let finding = state
                .workflows
                .findings
                .get(&a.finding)
                .map_err(fail)?
                .ok_or_else(|| invalid(format!("Finding {} does not exist.", a.finding)))?;
            let mut finding = finding;
            if !matches!(finding.state.as_str(), "open" | "proposed" | "uncertain") {
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

/// Create the work item a finding's draft describes, once (`workflow_finding_approve` by the
/// person, who names the draft they saw by the finding's `updated_at`, or a blocking finding by
/// its workflow's lead within the caps): checked against the live type and the token first,
/// recorded `sending` before it is sent, then `item_created` with its link or `uncertain`, and
/// never sent again. The send outlives its caller (a dropped request, a lead's turn that ends).
pub(in crate::app) async fn create(
    host: &Arc<Host>,
    azure: &engine::Azure,
    id: &str,
    by: &str,
    seen: Option<i64>,
) -> Result<Value, String> {
    let state = &host.state;
    // One creation at a time on the account: a finding is never sent twice, and the caps are
    // counted with nothing else being created.
    let lock = state.workflows.lock("finding-create");
    let _one = lock.lock().await;
    let findings = &state.workflows.findings;
    let mut finding = findings
        .get(id)
        .map_err(fail)?
        .ok_or_else(|| invalid(format!("There is no finding {id}.")))?;
    if finding.state != "proposed" {
        return Err(conflict(format!(
            "Finding {id} is {}: only a proposed draft is created, once.",
            finding.state
        )));
    }
    if seen.is_some_and(|at| at != finding.updated_at) {
        return Err(conflict(format!(
            "The draft of finding {id} changed since you saw it: look at it again."
        )));
    }
    let draft = finding
        .draft
        .clone()
        .ok_or_else(|| conflict("The finding has no draft."))?;
    let run = state
        .workflows
        .db
        .get(&finding.run)
        .map_err(fail)?
        .ok_or_else(|| invalid("The finding's run is gone."))?;
    if by == "lead" {
        lead_caps(
            &findings.list(None, None, None, -1).map_err(fail)?,
            &finding,
            &run,
        )?;
    }
    let parent = azure
        .item(
            state,
            run.item.source.as_deref(),
            Some(&run.item.project),
            run.item.id,
        )
        .await?;
    azure.check_child(state, &run, &parent, &draft).await?;
    azure.ready_to_create(state, &run).await?;
    let now = chrono::Utc::now().timestamp();
    finding.state = "sending".into();
    finding.created = Some(json!({"by": by, "at": now}));
    finding.updated_at = now;
    findings.save(&finding).map_err(fail)?;
    let task = {
        let (host, azure, run, draft) = (host.clone(), azure.clone(), run.clone(), draft.clone());
        tokio::spawn(async move { azure.create_child(&host.state, &run, &parent, &draft).await })
    };
    let sent = task.await.unwrap_or_else(|_| Err("uncertain".into()));
    finding.updated_at = chrono::Utc::now().timestamp();
    let unsent = sent.as_ref().is_err_and(|e| e.contains("unsent: "));
    match &sent {
        Ok(item) => {
            finding.state = "item_created".into();
            finding.created =
                Some(json!({"id": item["id"], "url": item["url"], "by": by, "at": now}));
        }
        // Nothing left: it is a proposal again.
        Err(_) if unsent => {
            finding.state = "proposed".into();
            finding.created = None;
        }
        Err(_) => finding.state = "uncertain".into(),
    }
    findings.save(&finding).map_err(fail)?;
    match sent {
        Ok(_) => Ok(json!({"finding": finding})),
        Err(reason) if unsent => Err(format!(
            "The work item of finding {id} was not sent: {reason}"
        )),
        Err(_) => Err(format!(
            "The work item of finding {id} was not confirmed: it is never sent again; look in Azure DevOps whether it exists."
        )),
    }
}

/// The most blocking findings' work items a lead creates for one run, and for one workflow a day.
const BLOCKING_PER_RUN: usize = 1;
const BLOCKING_PER_DAY: usize = 3;

/// What a lead may create by itself: a blocking finding's item, at most one per run and three a
/// day per workflow (those it sent, confirmed or not), never for a run whose own item came from
/// a finding.
fn lead_caps(all: &[Finding], finding: &Finding, run: &Run) -> Result<(), String> {
    if finding.severity != "blocking" {
        return Err(conflict(
            "Only a blocking finding's work item is created without the person: propose it.",
        ));
    }
    let by_lead = |f: &&Finding| f.created.as_ref().is_some_and(|c| c["by"] == "lead");
    if all
        .iter()
        .filter(by_lead)
        .filter(|f| f.run == finding.run)
        .count()
        >= BLOCKING_PER_RUN
    {
        return Err(conflict(
            "This run has its blocking finding's work item already: propose this one for the person.",
        ));
    }
    let now = chrono::Utc::now().timestamp();
    let today = all
        .iter()
        .filter(by_lead)
        .filter(|f| f.workflow == finding.workflow)
        .filter(|f| {
            f.created
                .as_ref()
                .and_then(|c| c["at"].as_i64())
                .is_some_and(|at| now - at < 86_400)
        })
        .count();
    if today >= BLOCKING_PER_DAY {
        return Err(conflict(
            "This workflow had three blocking findings' work items in a day: propose this one for the person.",
        ));
    }
    if all.iter().any(|f| {
        f.created
            .as_ref()
            .is_some_and(|c| c["id"].as_u64() == Some(run.item.id))
    }) {
        return Err(conflict(
            "This run's item came from a finding: propose this one for the person.",
        ));
    }
    Ok(())
}
