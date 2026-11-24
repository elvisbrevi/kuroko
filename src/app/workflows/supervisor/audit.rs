//! An audit the retired workflow supervisor wrote of a batch of workflow runs, as stored in
//! `workflow_audits`, read with `workflow_supervision`.
use super::*;

/// At most this many audits were kept.
pub(in crate::app) const MAX_AUDITS: usize = 50;

/// One run as the audit lists it, from its record (and the supervisor's line about it).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Item {
    pub run: String,
    pub item: u64,
    pub title: String,
    /// The work item's verified link.
    pub url: String,
    /// Its project and organization, for a drafted work item's Create request.
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    pub state: String,
    pub children: usize,
    /// Each pull request of its children, with its status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_requests: Vec<Value>,
    /// Checks of its latest look's report claims that were not verified.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub not_verified: usize,
    /// The supervisor's line about it, cleaned.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub result: String,
}
fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

/// A branch whose pull request was completed and that still exists in its repository.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct MergedBranch {
    pub run: String,
    pub repository: String,
    pub branch: String,
    pub pull_request: u64,
}

/// A finding the supervisor read only in comments, reports or sessions.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Finding {
    pub run: String,
    pub text: String,
    /// `comment`, `report` or `session`.
    pub from: String,
    pub needs_item: bool,
}

/// A drafted work item of the audit, for the run it is about.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct AuditDraft {
    pub run: String,
    pub draft: Draft,
}

/// One audit, as stored in `workflow_audits`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(in crate::app) struct Audit {
    /// Its session's record in Agent history.
    pub id: String,
    /// `batch_ended` or `requested`.
    pub event: String,
    pub created_at: i64,
    /// The runs it covers were created after `since` and up to `until` (Unix seconds).
    pub since: i64,
    pub until: i64,
    pub supervised_by: Director,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// `done` or `failed`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncated: Vec<String>,
    pub items: Vec<Item>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_branches: Vec<MergedBranch>,
    /// Runs whose latest look failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_looks: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub overview: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work_items: Vec<AuditDraft>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u32,
    /// The runs of the audit that had not finished (`done` or `cancelled`): the next batch
    /// takes them again, so their real ending is audited too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<String>,
}
impl Audit {
    /// The audit in a list: its identity, when, what it covers and how it went.
    pub(in crate::app) fn summary(&self) -> Value {
        json!({
            "id": self.id, "event": self.event, "created_at": self.created_at,
            "since": self.since, "until": self.until, "outcome": self.outcome,
            "runs": self.items.len(), "findings": self.findings.len(),
            "merged_branches": self.merged_branches.len(),
        })
    }
}
