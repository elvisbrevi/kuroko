//! Every prompt the app gives a model, in one catalog: the implementation agents' instructions,
//! the rules and answer formats code adds to them, the workflow director's and supervisor's
//! rules, the contexts of workflow runs and review rounds, the Teams answers, the steps that read
//! a request, and the activity registration. The map keeps only what the user changed
//! (`[[agent_prompts]]`); a prompt not listed uses its default (`llm::prompts`). The cases'
//! descriptions and instructions (`crate::cases`) stay in `[[cases]]`. A prompt is text the
//! model reads, never a permission: code still checks every answer, audience, path, link and
//! limit, whatever a prompt says.

use crate::llm::prompts;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What the user changed in one built-in prompt.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    pub id: String,
    /// Replaces the default instructions.
    pub instructions: String,
}

/// A built-in prompt: where it is used and its default text.
#[derive(Clone, Debug, Serialize)]
pub struct BuiltIn {
    pub id: &'static str,
    /// The group the Prompts section lists it under (`SECTIONS`).
    pub section: &'static str,
    pub name: &'static str,
    /// When the model receives it, for the user.
    pub summary: &'static str,
    pub instructions: &'static str,
    /// The `{{name}}` values code fills in; the text may move or leave out any of them.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub placeholders: &'static [&'static str],
}

pub const PLAN: &str = "plan";
pub const CODE: &str = "code";
pub const REQUEST: &str = "request";
pub const DIRECT: &str = "direct";
pub const SUPERVISE: &str = "supervise";

/// The groups of the catalog, in the order the Prompts section shows them: the
/// implementation agents' own instructions first (To-Do's «Guides and instructions» edits the
/// same ones).
pub const SECTIONS: [&str; 7] = [
    "agents",
    "agent_rules",
    "workflow_director",
    "workflow_runs",
    "answers",
    "steps",
    "activity",
];

/// The agents' own instructions keep their earlier, shorter limit; every other prompt may be as
/// long as this.
const AGENT_LIMIT: usize = 8000;
const LIMIT: usize = 20_000;

macro_rules! built_in {
    ($id:expr, $section:expr, $name:expr, $summary:expr, $text:expr $(, [$($p:expr),* $(,)?])? $(,)?) => {
        BuiltIn {
            id: $id,
            section: $section,
            name: $name,
            summary: $summary,
            instructions: $text,
            placeholders: &[$($($p),*)?],
        }
    };
}

pub const BUILT_IN: &[BuiltIn] = &[
    built_in!(
        PLAN,
        "agents",
        "Plan children",
        "Implement on an HU, a bug or a GitHub epic: the agent proposes child tasks for you to review before creating them.",
        prompts::IMPLEMENTATION_PLAN
    ),
    built_in!(
        CODE,
        "agents",
        "Implement",
        "Implement on a task, a development bug or a GitHub issue: the agent works with full permissions on the chosen computer, in the selected directories, until the request is done.",
        prompts::IMPLEMENTATION_CODE
    ),
    built_in!(
        REQUEST,
        "agents",
        "Write a request",
        "Create request in To-Do: the agent writes the new HU, bug or GitHub epic from your context.",
        prompts::IMPLEMENTATION_REQUEST
    ),
    built_in!(
        DIRECT,
        "agents",
        "Choose a workflow",
        "Workflows: the workflow director reads a work item and the enabled workflows of its project and chooses the one that fits, or none.",
        prompts::WORKFLOW_DIRECT
    ),
    built_in!(
        SUPERVISE,
        "agents",
        "Supervise workflow runs",
        "Workflows: the workflow supervisor reads what a run's agents did, checks their reports and drafts a handover note, work items and the next steps for you to confirm.",
        prompts::WORKFLOW_SUPERVISE
    ),
    // What code adds to the agents' instructions.
    built_in!(
        "rules.plan",
        "agent_rules",
        "Planning rules",
        "Added after the planning instructions: what the agent may trust and do.",
        prompts::PLAN_RULES
    ),
    built_in!(
        "rules.plan_order",
        "agent_rules",
        "Order of planned children",
        "Added to every plan: how children are ordered and declare what they depend on.",
        prompts::PLAN_ORDER
    ),
    built_in!(
        "rules.request",
        "agent_rules",
        "Request rules",
        "Added after the request instructions: what the agent may trust and do.",
        prompts::REQUEST_RULES
    ),
    built_in!(
        "rules.code",
        "agent_rules",
        "Coding rules",
        "Added after the coding instructions: untrusted evidence and a run nobody answers.",
        prompts::CODE_RULES
    ),
    built_in!(
        "rules.forbidden",
        "agent_rules",
        "Coding prohibitions",
        "The sentence that lists what a coding run may not do; actions is the list of prohibitions its workflow does not lift.",
        prompts::FORBIDDEN,
        ["actions"]
    ),
    built_in!(
        "rules.forbid_work_items",
        "agent_rules",
        "Prohibition: work items",
        "One prohibition of a coding run, lifted by a workflow that allows work items.",
        prompts::FORBID_WORK_ITEMS
    ),
    built_in!(
        "rules.forbid_teams",
        "agent_rules",
        "Prohibition: Teams",
        "One prohibition of a coding run, lifted by a workflow that allows Teams.",
        prompts::FORBID_TEAMS
    ),
    built_in!(
        "rules.forbid_merge",
        "agent_rules",
        "Prohibition: merge",
        "One prohibition of a coding run, lifted by a workflow that allows merging.",
        prompts::FORBID_MERGE
    ),
    built_in!(
        "rules.forbid_publish",
        "agent_rules",
        "Prohibition: publish",
        "One prohibition of a coding run, lifted by a workflow that allows deploying.",
        prompts::FORBID_PUBLISH
    ),
    built_in!(
        "rules.forbid_deploy",
        "agent_rules",
        "Prohibition: deploy",
        "One prohibition of a coding run, lifted by a workflow that allows deploying.",
        prompts::FORBID_DEPLOY
    ),
    built_in!(
        "rules.allowed",
        "agent_rules",
        "What a workflow allows",
        "Added when the workflow that started a coding run lifts prohibitions; allowed lists them.",
        prompts::ALLOWED,
        ["allowed"]
    ),
    built_in!(
        "rules.time_limit",
        "agent_rules",
        "Time limit",
        "Added to the rules of a coding run and its review rounds: minutes is the run's time limit and stops_at when it ends (UTC).",
        prompts::TIME_LIMIT,
        ["minutes", "stops_at"]
    ),
    built_in!(
        "rules.json",
        "agent_rules",
        "JSON answer",
        "Asks planning, request, director and supervisor sessions for their JSON answer; schema is the answer's JSON schema.",
        prompts::JSON_CONTRACT,
        ["schema"]
    ),
    built_in!(
        "rules.opencode_planning",
        "agent_rules",
        "OpenCode: planning agent",
        "The system text of OpenCode's agent in planning and request runs.",
        prompts::OPENCODE_PLANNING
    ),
    built_in!(
        "rules.opencode_coding",
        "agent_rules",
        "OpenCode: coding agent",
        "The system text of OpenCode's agent in coding runs, before its prohibitions.",
        prompts::OPENCODE_CODING
    ),
    // The workflow director and supervisor.
    built_in!(
        "director.rules",
        "workflow_director",
        "Director: choosing a workflow",
        "Added after the director's instructions when it chooses a workflow for a work item.",
        prompts::DIRECT_RULES
    ),
    built_in!(
        "director.summary_task",
        "workflow_director",
        "Director: progress summary",
        "Added to the director's instructions when it writes a run's progress summary.",
        prompts::SUMMARIZE_TASK
    ),
    built_in!(
        "director.summary_rules",
        "workflow_director",
        "Director: summary rules",
        "Added after the summary task: what the director may trust and write.",
        prompts::SUMMARY_RULES
    ),
    built_in!(
        "supervisor.rules",
        "workflow_director",
        "Supervisor: looking at runs",
        "Added after the supervisor's instructions when it looks at runs.",
        prompts::SUPERVISE_RULES
    ),
    built_in!(
        "supervisor.audit_task",
        "workflow_director",
        "Supervisor: audit",
        "Added to the supervisor's instructions when it audits a batch of runs.",
        prompts::AUDIT_TASK
    ),
    built_in!(
        "supervisor.audit_rules",
        "workflow_director",
        "Supervisor: audit rules",
        "Added after the audit task: what the supervisor may trust and write.",
        prompts::AUDIT_RULES
    ),
    // The contexts of workflow runs and review rounds.
    built_in!(
        "runs.workflow",
        "workflow_runs",
        "Workflow context",
        "Opens the workflow's part of a run's context, before its repositories.",
        prompts::RUN_WORKFLOW,
        ["name", "id", "project"]
    ),
    built_in!(
        "runs.repository",
        "workflow_runs",
        "Repository line",
        "One repository of the workflow and its checkout on the machine.",
        prompts::RUN_REPOSITORY,
        ["repository", "directory"]
    ),
    built_in!(
        "runs.instructions",
        "workflow_runs",
        "Workflow instructions heading",
        "Comes before the workflow's own instructions.",
        prompts::RUN_INSTRUCTIONS
    ),
    built_in!(
        "runs.block",
        "workflow_runs",
        "Instruction block heading",
        "Comes before each instruction block of the workflow.",
        prompts::RUN_BLOCK,
        ["name"]
    ),
    built_in!(
        "runs.opening_one",
        "workflow_runs",
        "Coding run: one repository",
        "How a coding run of a workflow with one repository starts; parent is the next prompt, for a planned child.",
        prompts::RUN_OPENING_ONE,
        [
            "kind",
            "id",
            "title",
            "url",
            "parent",
            "repository",
            "directory"
        ]
    ),
    built_in!(
        "runs.opening_many",
        "workflow_runs",
        "Coding run: several repositories",
        "How a coding run of a workflow with several repositories starts; others lists the other checkouts.",
        prompts::RUN_OPENING_MANY,
        [
            "kind",
            "id",
            "title",
            "url",
            "repository",
            "directory",
            "others"
        ]
    ),
    built_in!(
        "runs.child_of",
        "workflow_runs",
        "Parent of a planned child",
        "Names the parent of a planned child in the opening.",
        prompts::RUN_CHILD_OF,
        ["parent"]
    ),
    built_in!(
        "runs.other_checkout",
        "workflow_runs",
        "Other checkout",
        "Names each other repository's checkout in the opening.",
        prompts::RUN_OTHER_CHECKOUT,
        ["repository", "directory"]
    ),
    built_in!(
        "runs.dependency_links",
        "workflow_runs",
        "Dependencies between created work items",
        "Added when the workflow allows work items: how the run links one that needs another first.",
        prompts::RUN_DEPENDENCY_LINKS
    ),
    built_in!(
        "runs.stacked",
        "workflow_runs",
        "Stacked work",
        "Added for a child stacked on another's open pull request.",
        prompts::RUN_STACKED,
        ["branch", "url"]
    ),
    built_in!(
        "runs.handover_person",
        "workflow_runs",
        "Your handover note",
        "Carries the note you wrote when you restarted a run.",
        prompts::RUN_HANDOVER_PERSON,
        ["note"]
    ),
    built_in!(
        "runs.handover_drafted",
        "workflow_runs",
        "Supervisor's handover, confirmed",
        "Carries the supervisor's handover note you confirmed unedited.",
        prompts::RUN_HANDOVER_DRAFTED,
        ["note"]
    ),
    built_in!(
        "runs.handover_preapproved",
        "workflow_runs",
        "Supervisor's handover, pre-approved",
        "Carries the supervisor's handover note a pre-approval applied.",
        prompts::RUN_HANDOVER_PREAPPROVED,
        ["note"]
    ),
    built_in!(
        "reviews.round",
        "workflow_runs",
        "Review round",
        "The request of a review round after a reviewer asked for changes; data holds the reviewers, votes and open threads.",
        prompts::REVIEW_ROUND,
        [
            "n",
            "kind",
            "id",
            "url",
            "data",
            "branch",
            "target",
            "repository",
            "pr",
            "heading",
            "support"
        ]
    ),
    built_in!(
        "reviews.support",
        "workflow_runs",
        "Review-support task step",
        "The review round's step for a review-support task, when the workflow allows work items.",
        prompts::REVIEW_SUPPORT
    ),
    built_in!(
        "reviews.conflict",
        "workflow_runs",
        "Conflict round",
        "The request of a round for a pull request that conflicts with its target; files is the next prompt.",
        prompts::REVIEW_CONFLICT,
        [
            "n", "url", "kind", "id", "target", "files", "branch", "heading"
        ]
    ),
    built_in!(
        "reviews.files",
        "workflow_runs",
        "Conflicting files",
        "Names the conflicting files in a conflict round.",
        prompts::REVIEW_FILES,
        ["files"]
    ),
    built_in!(
        "reviews.handover",
        "workflow_runs",
        "Review round takeover",
        "Told to a new session when the one that coded the child cannot be continued.",
        prompts::REVIEW_HANDOVER,
        ["branch", "url"]
    ),
    built_in!(
        "reviews.handover_report",
        "workflow_runs",
        "Earlier session's report",
        "Introduces the end of that session's final report.",
        prompts::REVIEW_HANDOVER_REPORT
    ),
    // Teams answers.
    built_in!(
        "answers.frame",
        "answers",
        "Answer rules",
        "Every Teams answer is written under these rules; the cases' instructions and your style fill them in.",
        prompts::ANSWER_FRAME,
        [
            "language",
            "general",
            "teammate",
            "status_round",
            "recap",
            "documentation",
            "style"
        ]
    ),
    built_in!(
        "answers.review",
        "answers",
        "Activity review",
        "Comes before the activity review case's instructions.",
        prompts::ANSWER_REVIEW
    ),
    built_in!(
        "answers.review_proposing",
        "answers",
        "Activity review, with proposals",
        "Closes an activity review when the app proposes where to register the work.",
        prompts::ANSWER_REVIEW_PROPOSING
    ),
    built_in!(
        "answers.review_reading",
        "answers",
        "Activity review, read only",
        "Closes an activity review when the app only reads existing tasks.",
        prompts::ANSWER_REVIEW_READING
    ),
    built_in!(
        "answers.in_progress",
        "answers",
        "Work in progress",
        "Added when the evidence is your own sessions with coding agents. Whatever it says, the app withholds an answer that names a person other than the one it replies to, or a work item.",
        prompts::IN_PROGRESS
    ),
    built_in!(
        "answers.recipient",
        "answers",
        "Note about the person",
        "Added when you keep a note about the person who wrote.",
        prompts::ANSWER_RECIPIENT
    ),
    built_in!(
        "answers.evaluations",
        "answers",
        "Your ratings",
        "Added when you rated earlier answers to the same question.",
        prompts::ANSWER_EVALUATIONS
    ),
    built_in!(
        "answers.examples",
        "answers",
        "Approved examples",
        "Added when you approved answers to similar requests.",
        prompts::ANSWER_EXAMPLES
    ),
    built_in!(
        "answers.context",
        "answers",
        "Context instructions",
        "Added when the request's context has its own instructions.",
        prompts::ANSWER_CONTEXT
    ),
    built_in!(
        "answers.previous_draft",
        "answers",
        "Improving a draft",
        "Added when you ask to improve an answer that was not sent.",
        prompts::ANSWER_PREVIOUS_DRAFT
    ),
    built_in!(
        "answers.guidance",
        "answers",
        "Your guidance",
        "Added with your own guidance or corrections for an answer.",
        prompts::ANSWER_GUIDANCE
    ),
    built_in!(
        "answers.previous_draft_keep",
        "answers",
        "Keeping what was right",
        "Added after your guidance when an unsent answer is being improved.",
        prompts::ANSWER_PREVIOUS_DRAFT_KEEP
    ),
    built_in!(
        "answers.contract",
        "answers",
        "Answer format",
        "Closes every Teams answer's prompt: the JSON answer and how links and names are written.",
        prompts::ANSWER_CONTRACT
    ),
    built_in!(
        "answers.questions_status_round",
        "answers",
        "Status round, with questions",
        "Replaces the status round rule once you configured questions.",
        prompts::QUESTIONS_STATUS_ROUND
    ),
    built_in!(
        "answers.questions_recap",
        "answers",
        "Recap, with questions",
        "Replaces the recap rule once you configured questions.",
        prompts::QUESTIONS_RECAP
    ),
    // How the assistant reads a request.
    built_in!(
        "steps.classify",
        "steps",
        "Classify the message",
        "Decides whether a message is a question, an activity review, a personal request, a greeting or a statement; the categories are the cases' descriptions.",
        prompts::CLASSIFY,
        ["question", "activity_review", "personal", "statement"]
    ),
    built_in!(
        "steps.standalone",
        "steps",
        "Standalone request",
        "Rewrites a follow-up as a request that names its own topic, before searching.",
        prompts::STANDALONE
    ),
    built_in!(
        "steps.select_source",
        "steps",
        "Choose a source",
        "Chooses the one read-only source to search for a request.",
        prompts::SELECT_SOURCE
    ),
    built_in!(
        "steps.select_question",
        "steps",
        "Match a question",
        "Matches a message to one of your configured questions.",
        prompts::SELECT_QUESTION
    ),
    built_in!(
        "steps.select_context",
        "steps",
        "Choose a context",
        "Chooses which of your contexts a request is about.",
        prompts::SELECT_CONTEXT
    ),
    built_in!(
        "steps.select_sessions",
        "steps",
        "Choose agent sessions",
        "Chooses which of your own agent sessions a request refers to.",
        prompts::SELECT_SESSIONS
    ),
    built_in!(
        "steps.select_references",
        "steps",
        "Choose references",
        "Chooses which verified references a drafted answer uses, so the app links them.",
        prompts::SELECT_REFERENCES
    ),
    built_in!(
        "steps.condense",
        "steps",
        "Shorten a message",
        "Shortens a Teams message that does not fit in one card; max_chars is the most it may have.",
        prompts::CONDENSE,
        ["max_chars"]
    ),
    // Activity registration.
    built_in!(
        "activity.plan_tasks",
        "activity",
        "Plan the day's tasks",
        "Drafts the day's tasks from your performed work, for you to review.",
        prompts::PLAN_ACTIVITY,
        ["language"]
    ),
    built_in!(
        "activity.propose_registration",
        "activity",
        "Propose where to register",
        "Proposes where to register work that has no linked work item.",
        prompts::PROPOSE_REGISTRATION,
        ["language"]
    ),
    built_in!(
        "activity.plan_links",
        "activity",
        "Read your reply to a review",
        "Turns your reply to an activity review into the links it asks for.",
        prompts::PLAN_LINKS
    ),
];

fn built_in(id: &str) -> Option<&'static BuiltIn> {
    BUILT_IN.iter().find(|b| b.id == id)
}

/// The prompts in effect: the user's changes over the defaults. Each machine resolves a prompt
/// it is not given with its own default, so a run on another machine carries only the changes
/// it needs (`only`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(transparent)]
pub struct Texts(BTreeMap<String, String>);
impl Texts {
    pub fn new(changes: &[Prompt]) -> Self {
        Self(
            changes
                .iter()
                .filter(|c| built_in(&c.id).is_some())
                .map(|c| (c.id.clone(), c.instructions.clone()))
                .collect(),
        )
    }
    /// The text in effect for `id`; empty for a prompt this build does not know.
    pub fn get(&self, id: &str) -> String {
        self.0
            .get(id)
            .cloned()
            .or_else(|| built_in(id).map(|b| b.instructions.to_owned()))
            .unwrap_or_default()
    }
    /// The text for `id` with its `{{name}}` values filled in, in one pass: a value that itself
    /// holds `{{…}}` is never filled again. A name the prompt does not declare stays as written.
    pub fn render(&self, id: &str, values: &[(&str, &str)]) -> String {
        render(&self.get(id), values)
    }
    /// Only the changes to these prompts, for a machine that resolves the rest itself.
    pub fn only(&self, ids: &[&str]) -> Self {
        Self(
            self.0
                .iter()
                .filter(|(id, _)| ids.contains(&id.as_str()))
                .map(|(id, text)| (id.clone(), text.clone()))
                .collect(),
        )
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// The changes another machine sent: only known prompts within their limits, as `validate`
    /// accepts them.
    pub fn check(&self) -> Result<()> {
        let changes: Vec<Prompt> = self
            .0
            .iter()
            .map(|(id, text)| Prompt {
                id: id.clone(),
                instructions: text.clone(),
            })
            .collect();
        validate(&changes)
    }
}

/// `text` with each declared `{{name}}` of `values` replaced, in one left-to-right pass.
pub fn render(text: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let value = after.find("}}").and_then(|end| {
            let name = &after[..end];
            values
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| (*v, end))
        });
        match value {
            Some((value, end)) => {
                out.push_str(value);
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The instructions in effect for a built-in prompt.
pub fn resolve(changes: &[Prompt], id: &str) -> String {
    Texts::new(changes).get(id)
}

pub fn validate(changes: &[Prompt]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for change in changes {
        let known = built_in(&change.id);
        ensure!(
            known.is_some(),
            "unknown prompt {}; list them with kuroko prompts list",
            change.id
        );
        ensure!(
            seen.insert(change.id.as_str()),
            "duplicate prompt {}",
            change.id
        );
        let limit = if known.is_some_and(|b| b.section == "agents") {
            AGENT_LIMIT
        } else {
            LIMIT
        };
        ensure!(
            !change.instructions.trim().is_empty()
                && change.instructions.chars().count() <= limit
                && !change
                    .instructions
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t'),
            "the prompt {} needs a text of at most {limit} characters",
            change.id
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_replaces_only_its_prompt() {
        let changes = [Prompt {
            id: CODE.into(),
            instructions: "Follow the repository's AGENTS.md.".into(),
        }];
        assert!(validate(&changes).is_ok());
        assert_eq!(
            resolve(&changes, CODE),
            "Follow the repository's AGENTS.md."
        );
        assert_eq!(resolve(&changes, PLAN), prompts::IMPLEMENTATION_PLAN);
        let change = |id: &str, text: &str| Prompt {
            id: id.into(),
            instructions: text.into(),
        };
        assert!(validate(&[change("unknown", "x")]).is_err());
        assert!(validate(&[change(PLAN, " ")]).is_err());
        assert!(validate(&[change(PLAN, "a"), change(PLAN, "b")]).is_err());
        assert!(validate(&[change(REQUEST, &"x".repeat(8001))]).is_err());
        // Every other prompt may be longer than an agent's instructions.
        assert!(validate(&[change("answers.frame", &"x".repeat(8001))]).is_ok());
        assert!(validate(&[change("answers.frame", &"x".repeat(20_001))]).is_err());
    }

    #[test]
    fn the_director_has_its_own_editable_prompt() {
        assert_eq!(resolve(&[], DIRECT), prompts::WORKFLOW_DIRECT);
        let edited = [Prompt {
            id: DIRECT.into(),
            instructions: "Prefer the workflow that names the item's repository.".into(),
        }];
        assert!(validate(&edited).is_ok());
        assert_eq!(
            resolve(&edited, DIRECT),
            "Prefer the workflow that names the item's repository."
        );
        assert_eq!(resolve(&edited, PLAN), prompts::IMPLEMENTATION_PLAN);
        let limit = [Prompt {
            id: DIRECT.into(),
            instructions: "x".repeat(8000),
        }];
        assert!(validate(&limit).is_ok());
        let over = [Prompt {
            id: DIRECT.into(),
            instructions: "x".repeat(8001),
        }];
        assert!(validate(&over).is_err());
    }

    #[test]
    fn the_supervisor_has_its_own_editable_prompt() {
        assert_eq!(resolve(&[], SUPERVISE), prompts::WORKFLOW_SUPERVISE);
        let edited = [Prompt {
            id: SUPERVISE.into(),
            instructions: "Be brief.".into(),
        }];
        assert!(validate(&edited).is_ok());
        assert_eq!(resolve(&edited, SUPERVISE), "Be brief.");
        assert_eq!(resolve(&edited, DIRECT), prompts::WORKFLOW_DIRECT);
    }

    #[test]
    fn the_catalog_is_whole_and_declares_its_values() {
        let mut ids = std::collections::BTreeSet::new();
        for prompt in BUILT_IN {
            assert!(ids.insert(prompt.id), "{}", prompt.id);
            assert!(SECTIONS.contains(&prompt.section), "{}", prompt.id);
            assert!(!prompt.instructions.trim().is_empty(), "{}", prompt.id);
            // Every `{{name}}` the default holds is declared, and every declared one is used.
            let mut used = std::collections::BTreeSet::new();
            let mut rest = prompt.instructions;
            while let Some(start) = rest.find("{{") {
                let after = &rest[start + 2..];
                let end = after.find("}}").unwrap();
                used.insert(&after[..end]);
                rest = &after[end + 2..];
            }
            let declared: std::collections::BTreeSet<_> =
                prompt.placeholders.iter().copied().collect();
            assert_eq!(used, declared, "{}", prompt.id);
        }
    }

    #[test]
    fn rendering_fills_each_value_once() {
        assert_eq!(
            render("a {{x}} b {{y}} {{z}}", &[("x", "{{y}}"), ("y", "2")]),
            "a {{y}} b 2 {{z}}"
        );
        assert_eq!(render("{{x", &[("x", "1")]), "{{x");
        let texts = Texts::new(&[Prompt {
            id: "steps.condense".into(),
            instructions: "At most {{max_chars}}.".into(),
        }]);
        assert_eq!(
            texts.render("steps.condense", &[("max_chars", "40")]),
            "At most 40."
        );
        assert_eq!(texts.only(&["steps.classify"]), Texts::default());
        assert!(texts.check().is_ok());
    }
}
