//! Workflow leads: agents the user names and assigns to workflows. A lead follows the runs of
//! its workflows with their context (descriptions, completion criteria, instructions, blocks);
//! this module holds the definitions and their rules, checked whenever the map is read. A
//! workflow names its lead in `Workflow::lead`. A definition grants nothing by itself.
use super::{Agent, Workflow, line, plain, valid_id};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// At most this many leads.
pub const MAX_LEADS: usize = 20;
/// A lead's name, at most.
pub const MAX_NAME: usize = 60;
/// A lead's description, at most.
pub const MAX_DESCRIPTION: usize = 2000;
/// A workflow's completion criteria, at most.
pub const MAX_DONE_WHEN: usize = 4000;

/// A workflow lead as the user defines it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Lead {
    pub id: String,
    /// The user's name for it («Pagos»): how it is called on every surface.
    pub name: String,
    /// What the user wants from it, part of its context.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Its agent: Claude Code on the coordinator, with this model and effort.
    pub agent: Agent,
    /// On unless the user turns it off.
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default)]
    pub updated_at: i64,
}
fn on() -> bool {
    true
}

/// A workflow's lead.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowLead {
    /// The lead's ID.
    pub id: String,
    /// What it may do for this workflow's runs by itself, once out of shadow (0.20.9).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub may: Vec<May>,
    /// Its decisions are only recorded (the default); false applies what `may` lists.
    #[serde(default = "on", skip_serializing_if = "is_on")]
    pub shadow: bool,
}
impl Default for WorkflowLead {
    fn default() -> Self {
        Self {
            id: String::new(),
            may: Vec::new(),
            shadow: true,
        }
    }
}
impl WorkflowLead {
    /// Whether its decision `what` applies to this workflow's runs.
    pub fn applies(&self, what: May) -> bool {
        !self.shadow && self.may.contains(&what)
    }
}
fn is_on(value: &bool) -> bool {
    *value
}

/// What a workflow's lead may do for its runs without asking.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum May {
    /// Settle a child with deliveries code verifies (as accepting what was found).
    Settle,
    /// Close a run by the workflow's completion criteria, above the floor code enforces.
    Close,
    /// Keep waiting for a run's pull requests past the account's limit (up to 60 days from the
    /// wait's start), and move a run ahead in the line.
    Wait,
    /// Continue a coding session cut before its final report, in its own conversation, at most
    /// twice per child.
    Continue,
}
impl May {
    /// What allowing it means, for the confirmation of a workflow's save.
    pub fn describe(self) -> &'static str {
        match self {
            May::Settle => {
                "settle a child with the branches, pull requests or Wiki pages code verifies it delivered, after which the run goes on by itself (its next child is coded)"
            }
            May::Continue => {
                "continue a coding session that was cut before its final report, in its own conversation with its note, at most twice per child, never while it runs or under a pause"
            }
            May::Wait => {
                "keep waiting for a run's pull requests past the account's limit, up to 60 days from the start of the wait (without its decision a day after the limit, the run stops as before), and move a run ahead in the line"
            }
            May::Close => {
                "close a run as done by the workflow's completion criteria once every child is settled and no session is alive, also with pull requests still open (their pull-requests-completed blocks are skipped)"
            }
        }
    }
}

/// A name compared as people read it: without letter case, accents (also typed as separate
/// combining marks) or repeated spaces.
pub fn name_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .chars()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            other => other,
        })
        .collect()
}

/// The lead `reference` names: its ID, or its name as people read it.
pub fn find<'a>(leads: &'a [Lead], reference: &str) -> Option<&'a Lead> {
    leads.iter().find(|l| l.id == reference).or_else(|| {
        leads
            .iter()
            .find(|l| name_key(&l.name) == name_key(reference))
    })
}

/// The workflows that name lead `id`.
pub fn led_by<'a>(workflows: &'a [Workflow], id: &str) -> Vec<&'a str> {
    workflows
        .iter()
        .filter(|w| w.lead.as_ref().is_some_and(|l| l.id == id))
        .map(|w| w.id.as_str())
        .collect()
}

/// One lead's own rules.
pub fn validate_lead(lead: &Lead) -> Result<()> {
    let id = &lead.id;
    ensure!(
        valid_id(id),
        "A lead ID is lowercase letters, digits, - and _ (up to 64) and starts with a letter."
    );
    ensure!(
        line(&lead.name, MAX_NAME) && !lead.name.chars().any(formatting),
        "Lead {id} needs a name of at most {MAX_NAME} characters, without control or formatting characters."
    );
    ensure!(
        plain(&lead.description, MAX_DESCRIPTION),
        "The description of lead {id} takes at most {MAX_DESCRIPTION} characters."
    );
    ensure!(
        lead.agent.provider == "claude",
        "Lead {id} runs Claude Code: its provider is claude."
    );
    ensure!(
        line(&lead.agent.model, 120) && line(&lead.agent.effort, 40),
        "Lead {id} needs its agent's model and effort."
    );
    ensure!(
        lead.agent.time_limit_minutes.is_none(),
        "Lead {id} has no time limit of its own."
    );
    Ok(())
}

/// Zero-width and bidirectional formatting characters, which would make two names look alike.
fn formatting(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// Every lead and every workflow's lead and completion criteria.
pub fn validate(leads: &[Lead], workflows: &[Workflow]) -> Result<()> {
    ensure!(
        leads.len() <= MAX_LEADS,
        "At most {MAX_LEADS} workflow leads (there are {}).",
        leads.len()
    );
    let (mut ids, mut names) = (BTreeSet::new(), BTreeSet::new());
    for lead in leads {
        validate_lead(lead)?;
        ensure!(ids.insert(&lead.id), "Lead {} is listed twice.", lead.id);
        ensure!(
            names.insert(name_key(&lead.name)),
            "Two leads are called {}: give each its own name.",
            lead.name
        );
    }
    // A name is never another lead's ID, so a lead found by either is the one meant.
    for lead in leads {
        ensure!(
            !leads
                .iter()
                .any(|other| other.id != lead.id && other.id == name_key(&lead.name)),
            "Lead {} is called like another lead's ID; give it another name.",
            lead.id
        );
    }
    for workflow in workflows {
        if let Some(lead) = &workflow.lead {
            ensure!(
                ids.contains(&lead.id),
                "Workflow {} names lead {}, which does not exist.",
                workflow.id,
                lead.id
            );
        }
        ensure!(
            plain(&workflow.done_when, MAX_DONE_WHEN),
            "The completion criteria of workflow {} take at most {MAX_DONE_WHEN} characters.",
            workflow.id
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lead(id: &str, name: &str) -> Lead {
        Lead {
            id: id.into(),
            name: name.into(),
            agent: Agent {
                provider: "claude".into(),
                model: "model".into(),
                effort: "high".into(),
                time_limit_minutes: None,
            },
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn leads_have_their_own_names_and_workflows_name_existing_ones() {
        let pagos = lead("pagos", "Pagos");
        assert!(validate(std::slice::from_ref(&pagos), &[]).is_ok());
        // The same name as people read it is refused.
        assert!(validate(&[pagos.clone(), lead("otro", "  PÁGOS ")], &[]).is_err());
        assert!(validate(&[pagos.clone(), lead("otro", "Pa\u{301}gos")], &[]).is_err());
        // A name that is another lead's ID would make a reference ambiguous.
        assert!(validate(&[lead("pagos", "Antiguo"), lead("pagos-2", "pagos")], &[]).is_err());
        assert_eq!(
            find(std::slice::from_ref(&pagos), "págos").map(|l| l.id.as_str()),
            Some("pagos")
        );
        assert_eq!(
            find(std::slice::from_ref(&pagos), "pagos").map(|l| l.id.as_str()),
            Some("pagos")
        );
        for bad in [
            lead("Pagos", "Pagos"),
            lead("pagos", ""),
            lead("pagos", &"x".repeat(61)),
            lead("pagos", "Pa\u{202E}gos"),
            Lead {
                agent: Agent {
                    provider: "codex".into(),
                    ..pagos.agent.clone()
                },
                ..pagos.clone()
            },
        ] {
            assert!(validate_lead(&bad).is_err(), "{bad:?}");
        }
        let mut workflow = Workflow {
            id: "payments".into(),
            lead: Some(WorkflowLead {
                id: "missing".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            validate(
                std::slice::from_ref(&pagos),
                std::slice::from_ref(&workflow)
            )
            .is_err()
        );
        workflow.lead = Some(WorkflowLead {
            id: "pagos".into(),
            ..Default::default()
        });
        workflow.done_when = "x".repeat(MAX_DONE_WHEN + 1);
        assert!(
            validate(
                std::slice::from_ref(&pagos),
                std::slice::from_ref(&workflow)
            )
            .is_err()
        );
        workflow.done_when = "Done once its pull request is open for review.".into();
        assert!(validate(&[pagos], std::slice::from_ref(&workflow)).is_ok());
        assert_eq!(led_by(&[workflow], "pagos"), ["payments"]);
    }
}
