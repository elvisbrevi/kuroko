//! The workflow operations shared by the GUI, the CLI and MCP: definitions and blocks
//! (`crate::workflows`) listed, checked, saved and removed on the account's coordinator, and
//! the project's Git repository catalog they are chosen from.
//!
//! Saving a workflow checks it against what the host can read: the project in the enabled Azure
//! source's configured organizations, every repository in the project's catalog, every checkout
//! inside the machine's allowed roots (the machine inspects it once with `checkout_inspect`), its
//! `origin` naming that repository, and the coding agent installed there. What cannot be read
//! (an offline or older machine, a checkout without an origin) is a warning in the reply; what
//! is wrong refuses the save. Blocks that act are authorized when saved: a Teams notification
//! needs confirmation and the reply echoes its chat and template; a launcher block needs the
//! owner's authority (this computer's local page or the owner's own portal session, as approving
//! a launcher does) and an approved launcher of the coordinator with that program digest.
//! Workflows are saved only on the coordinator, where their runs live; they reach the account's
//! other computers with its portable configuration. Other settings editors keep them as stored
//! (`save_settings`). The workflow director's settings and its dry-run classification are in
//! `director`.
use super::*;
use crate::agent_prompts::Texts;
use crate::workflows::lead::{self, Lead};
use crate::workflows::{self as core, Block, Facts, Inspection, Kind, Workflow};
use serde_json::{Value, json};

pub(super) const METHODS: [&str; 17] = [
    "workflows_list",
    "workflow_show",
    "workflow_save",
    "workflow_remove",
    "workflow_blocks_list",
    "workflow_block_save",
    "workflow_block_remove",
    "workflow_repositories",
    "workflow_leads_list",
    "workflow_lead_show",
    "workflow_lead_save",
    "workflow_lead_remove",
    "workflow_lead_shifts",
    "workflow_lead_wake",
    "workflow_lead_answer",
    "workflow_lead_connect",
    "workflow_lead_disconnect",
];

pub(super) mod acting;
pub(in crate::app) mod archive;
pub(super) mod dependencies;
mod director;
pub(super) mod engine;
pub(super) mod findings;
pub(super) mod found;
pub(super) mod implement;
pub(super) mod record;
#[cfg(all(test, unix))]
mod rehearsal;
pub(super) mod review;
pub(super) mod runs;
pub(in crate::app) mod shifts;
pub(super) mod summary;
pub(super) mod supervisor;
#[cfg(test)]
pub(super) mod synthetic;
pub(super) mod watcher;

pub(super) fn handles(method: &str) -> bool {
    METHODS.contains(&method)
        || director::METHODS.contains(&method)
        || runs::METHODS.contains(&method)
        || summary::METHODS.contains(&method)
        || supervisor::METHODS.contains(&method)
}
/// The workflow supervisor's operations (`workflow_supervisor_support`).
pub(super) fn supervisor_handles(method: &str) -> bool {
    supervisor::METHODS.contains(&method)
}
/// The workflow run operations, and the summaries of a run (`workflow_runs_support`).
pub(super) fn runs_handles(method: &str) -> bool {
    runs::METHODS.contains(&method) || summary::METHODS.contains(&method)
}
/// The workflow director's operations (`workflow_director_support`).
pub(super) fn director_handles(method: &str) -> bool {
    director::METHODS.contains(&method)
}

fn invalid(text: impl std::fmt::Display) -> String {
    format!("[invalid_input] {text}")
}
fn conflict(text: impl std::fmt::Display) -> String {
    format!("[conflict] {text}")
}

pub(super) async fn operate(host: &Arc<Host>, request: &control::Request) -> control::Reply {
    match operation(host, &request.method, &request.args).await {
        Ok(data) => control::Reply::success(data),
        Err(message) => control::failure(&message, false),
    }
}

fn parse<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, String> {
    serde_json::from_value(args.clone()).map_err(|e| invalid(format!("Invalid arguments: {e}.")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    #[serde(default)]
    workflow: Option<Value>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    /// The user confirmed what the workflow's `allow` lets its coding agents do.
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveArgs {
    id: String,
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeadArgs {
    lead: String,
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeadSaveArgs {
    lead: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockSaveArgs {
    block: Value,
    #[serde(default)]
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoriesArgs {
    project: String,
    #[serde(default)]
    organization: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Whether workflows are saved here: on the account's coordinator, never on a computer signed
/// in to one.
fn editable(state: &DesktopState) -> Result<bool, String> {
    Ok(implementation::link(state).map_err(fail)?.is_none())
}
fn require_coordinator(state: &DesktopState) -> Result<(), String> {
    if editable(state)? {
        Ok(())
    } else {
        Err("[not_ready] This computer is signed in to the account's coordinator, where workflows are saved and run: change them there (its local page, its portal or its kuroko). They reach this computer with the account's configuration.".into())
    }
}

/// A workflow as the operations show it: `allow` always present, empty when it allows nothing.
fn shown(workflow: &Workflow) -> Value {
    let mut value = serde_json::to_value(workflow).unwrap_or_default();
    value["allow"] = json!(workflow.allow);
    value["preapprove"] = json!(workflow.preapprove);
    value
}

/// The blocks a workflow lists that the map holds, in its order.
fn listed(workflow: &Workflow, blocks: &[Block]) -> Vec<Block> {
    workflow
        .blocks
        .iter()
        .filter_map(|id| blocks.iter().find(|b| &b.id == id).cloned())
        .collect()
}

/// How much of each limit a workflow uses: its instructions' characters, its blocks, and the
/// characters of the largest context a run of it can be given (`fixed` of them are not its
/// instructions or instruction blocks: the fixed sentences, its checkouts, the item and the
/// longest handover note).
pub(super) fn usage(workflow: &Workflow, blocks: &[Block], texts: &Texts) -> Value {
    let listed = listed(workflow, blocks);
    let context = engine::largest_context(workflow, &listed, texts);
    let fixed = context.saturating_sub(engine::instructions_part(workflow, &listed, texts));
    json!({
        "instructions": {"used": workflow.instructions.chars().count(), "limit": core::MAX_INSTRUCTIONS},
        "blocks": {"used": workflow.blocks.len(), "limit": core::MAX_WORKFLOW_BLOCKS},
        "context": {"used": context, "limit": core::MAX_CONTEXT, "fixed": fixed},
    })
}

/// Refuses a workflow whose largest run context would not fit, with the numbers: a run is
/// given its instructions and instruction blocks whole or not at all.
fn fits(workflow: &Workflow, blocks: &[Block], texts: &Texts) -> Result<(), String> {
    let listed = listed(workflow, blocks);
    let used = engine::largest_context(workflow, &listed, texts);
    if used <= core::MAX_CONTEXT {
        return Ok(());
    }
    let part = engine::instructions_part(workflow, &listed, texts);
    Err(invalid(format!(
        "A run of workflow {} could be given {used} characters of context, more than the {} a run takes (and a context is never cut): {part} of them are its instructions and instruction blocks, the other {} the fixed sentences, its checkouts, the item and the longest handover note. Shorten its instructions or instruction blocks by at least {} characters.",
        workflow.id,
        core::MAX_CONTEXT,
        used - part,
        used - core::MAX_CONTEXT
    )))
}

/// Refuses a map in which lead `id`'s opening would not fit, naming each part, unless the lead
/// is off or the opening does not grow from `before` (a lead already over the limit, through
/// a block or the organization, can still be turned off or trimmed).
// shortcut: a block's or the organization's save does not check the openings of the leads it
// grows; a turn whose message is too large fails and says so. Check them there too if needed.
fn lead_fits(
    host: &Host,
    map: &KnowledgeMap,
    id: &str,
    before: Option<&KnowledgeMap>,
) -> Result<(), String> {
    let size = |map: &KnowledgeMap| -> Option<(usize, Result<(), String>)> {
        let lead = map
            .workflow_leads
            .iter()
            .find(|l| l.id == id && l.enabled)?;
        let workflows: Vec<Workflow> = map
            .workflows
            .iter()
            .filter(|w| w.lead.as_ref().is_some_and(|l| l.id == id))
            .cloned()
            .collect();
        let texts = Texts::new(&map.agent_prompts);
        let parts = shifts::parts(host, map, &workflows, &texts);
        Some((
            shifts::opening_size(lead, &parts, &texts),
            shifts::opening_fits(lead, &parts, &texts),
        ))
    };
    let Some((used, fits)) = size(map) else {
        return Ok(());
    };
    match fits {
        Err(_) if before.and_then(size).is_some_and(|(was, _)| was >= used) => Ok(()),
        result => result.map_err(invalid),
    }
}

/// What a workflow larger than older hosts accept means for the account's other computers.
/// A workflow that pre-approves anything is refused by a host from before the supervisor
/// (`deny_unknown_fields`): the save names the machines of the account that report one.
async fn older_supervisors(host: &Arc<Host>, workflow: &Workflow) -> Option<String> {
    if workflow.preapprove.is_empty() {
        return None;
    }
    let older = implementation::machines_before_supervisor(host)
        .await
        .unwrap_or_default();
    (!older.is_empty()).then(|| {
        format!(
            "Workflow {} pre-approves actions of the workflow supervisor, which {} cannot read: until updated, {} the account's configuration while this workflow is in it.",
            workflow.id,
            older.join(", "),
            if older.len() == 1 { "it refuses" } else { "they refuse" }
        )
    })
}

/// A workflow with delivery settings is refused by a host from before them (0.14.1 or older,
/// `deny_unknown_fields`): the save names the account's machines that report one.
async fn older_delivery(host: &Arc<Host>, workflow: &Workflow) -> Option<String> {
    if workflow.delivery.is_default() {
        return None;
    }
    let older = implementation::machines_until(host, core::OLDER_DELIVERY_UNTIL)
        .await
        .unwrap_or_default();
    (!older.is_empty()).then(|| {
        format!(
            "Workflow {} has delivery settings, which hosts of release {} or older cannot read: {} {} the account's configuration while this workflow is in it (keeping {}) until updated.",
            workflow.id,
            core::OLDER_DELIVERY_UNTIL,
            older.join(", "),
            if older.len() == 1 { "refuses" } else { "refuse" },
            if older.len() == 1 {
                "the copy it has"
            } else {
                "the copies they have"
            }
        )
    })
}

/// Leads, a workflow's lead or its completion criteria are refused by a host from before them
/// (0.20.1 or older): the save names the account's machines that report one.
async fn older_leads(host: &Arc<Host>, what: &str) -> Option<String> {
    let older = implementation::machines_until(host, core::OLDER_LEADS_UNTIL)
        .await
        .unwrap_or_default();
    (!older.is_empty()).then(|| {
        format!(
            "{what}, which hosts of release {} or older cannot read: {} {} the account's configuration (keeping {}) until updated.",
            core::OLDER_LEADS_UNTIL,
            older.join(", "),
            if older.len() == 1 { "refuses" } else { "refuse" },
            if older.len() == 1 {
                "the copy it has"
            } else {
                "the copies they have"
            }
        )
    })
}

/// A workflow with its own session time limit is refused by a host from before it (0.19.1 or
/// older, `deny_unknown_fields`): the save names the account's machines that report one.
async fn older_time_limit(host: &Arc<Host>, workflow: &Workflow) -> Option<String> {
    workflow.agent.time_limit_minutes?;
    let older = implementation::machines_until(host, core::OLDER_TIME_LIMIT_UNTIL)
        .await
        .unwrap_or_default();
    (!older.is_empty()).then(|| {
        format!(
            "Workflow {} has its own session time limit, which hosts of release {} or older cannot read: {} {} the account's configuration while this workflow is in it (keeping {}) until updated.",
            workflow.id,
            core::OLDER_TIME_LIMIT_UNTIL,
            older.join(", "),
            if older.len() == 1 { "refuses" } else { "refuse" },
            if older.len() == 1 {
                "the copy it has"
            } else {
                "the copies they have"
            }
        )
    })
}

async fn older_hosts(host: &Arc<Host>, workflow: &Workflow) -> Option<String> {
    let characters = workflow.instructions.chars().count();
    if characters <= core::OLDER_MAX_INSTRUCTIONS
        && workflow.blocks.len() <= core::OLDER_MAX_WORKFLOW_BLOCKS
    {
        return None;
    }
    let older = implementation::machines_until(host, core::OLDER_LIMITS_UNTIL)
        .await
        .unwrap_or_default();
    let machines = if older.is_empty() {
        "None of the account's machines reports one.".to_owned()
    } else {
        format!(
            "These of the account's machines report one, or no version: {}.",
            older.join(", ")
        )
    };
    Some(format!(
        "Workflow {} has {} blocks and {characters} characters of instructions, more than hosts of release {} or older accept ({} blocks, {} characters): a computer of this account that runs one cannot take the account's configuration while this workflow is in it (it keeps the copy it has and reports the coordinator's configuration as invalid; its implementation runs work as before) until it is updated (kuroko update, or kuroko update --machine NAME from here). {machines}",
        workflow.id,
        workflow.blocks.len(),
        core::OLDER_LIMITS_UNTIL,
        core::OLDER_MAX_WORKFLOW_BLOCKS,
        core::OLDER_MAX_INSTRUCTIONS,
    ))
}

/// What a person confirms before a workflow whose coding agents may do more than the fixed
/// rules allow is saved; none when it allows nothing.
pub(super) fn describe_workflow(workflow: &Workflow) -> Option<String> {
    let lifted = core::describe_allow(&workflow.allow);
    let preapproved = core::describe_preapprove(&workflow.preapprove);
    if lifted.is_empty() && preapproved.is_empty() && workflow.lead.is_none() {
        return None;
    }
    let mut text = format!("Save workflow {}?", workflow.id);
    if let Some(lead) = &workflow.lead {
        text.push_str(&format!(
            " Its workflow lead {} follows its runs on this coordinator with its own Claude agent and that account's usage, in shadow: it reads them and records what it would decide, and changes nothing.",
            lead.id
        ));
    }
    if !lifted.is_empty() {
        text.push_str(&format!(
            " In every run it starts (also those that start by themselves), {lifted}, when its instructions ask for it. Its planning run, and every run without this workflow, keep every prohibition."
        ));
    }
    if !preapproved.is_empty() {
        text.push_str(&format!(
            " For its runs, {preapproved}; code checks every condition first, and anything else the supervisor proposes waits for you."
        ));
    }
    Some(text)
}

/// What a person confirms before a block that acts is saved.
pub(super) fn describe(block: &Block) -> String {
    let stage = block
        .stage
        .and_then(|s| serde_json::to_value(s).ok())
        .and_then(|s| s.as_str().map(str::to_owned))
        .unwrap_or_default();
    match block.kind {
        Kind::TeamsNotification => {
            let chat = block.chat.clone().unwrap_or_default();
            let mentions = if block.mentions.is_empty() {
                "no one".to_owned()
            } else {
                block.mentions.join(", ")
            };
            format!(
                "At stage {stage}, post «{}» to the Teams chat {} ({}), mentioning {mentions}, once each time that stage occurs in a run",
                block.template, chat.name, chat.id
            )
        }
        Kind::Launcher => {
            let launcher = block.launcher.clone().unwrap_or_default();
            format!(
                "At stage {stage}, run the coordinator's approved launcher {} (program digest {})",
                launcher.id, launcher.digest
            )
        }
        Kind::Instruction => format!("The agents read the instruction «{}»", block.name),
    }
}

async fn operation(host: &Arc<Host>, method: &str, args: &Value) -> Result<Value, String> {
    let state = &host.state;
    if director::METHODS.contains(&method) {
        return director::operation(host, method, args).await;
    }
    if runs::METHODS.contains(&method) {
        return runs::operation(host, method, args, &engine::Azure::Live).await;
    }
    if summary::METHODS.contains(&method) {
        return summary::operation(host, method, args).await;
    }
    if supervisor::METHODS.contains(&method) {
        return supervisor::operation(host, method, args, &engine::Azure::Live).await;
    }
    match method {
        "workflows_list" => {
            ensure_no_arguments(args)?;
            let map = state.settings.map().map_err(fail)?;
            let texts = Texts::new(&map.agent_prompts);
            let workflows: Vec<Value> = map.workflows.iter().map(shown).collect();
            let used: BTreeMap<&str, Value> = map
                .workflows
                .iter()
                .map(|w| (w.id.as_str(), usage(w, &map.workflow_blocks, &texts)))
                .collect();
            Ok(json!({"workflows":workflows,"usage":used,"editable":editable(state)?}))
        }
        "workflow_show" => {
            let a: IdArgs = parse(args)?;
            let map = state.settings.map().map_err(fail)?;
            let workflow = map
                .workflows
                .iter()
                .find(|w| w.id == a.id)
                .ok_or_else(|| invalid(format!("There is no workflow {}.", a.id)))?;
            let blocks: Vec<&Block> = workflow
                .blocks
                .iter()
                .filter_map(|id| map.workflow_blocks.iter().find(|b| &b.id == id))
                .collect();
            let settings = &map.workflow_settings;
            Ok(json!({
                "workflow": shown(workflow),
                "blocks": blocks,
                "usage": usage(workflow, &map.workflow_blocks, &Texts::new(&map.agent_prompts)),
                // The limit each session of its runs gets: its agent's own, else the settings'.
                "time_limit_minutes": {
                    "coding": settings.time_limit_minutes(Some(workflow), false),
                    "planning": settings.time_limit_minutes(Some(workflow), true),
                    "from": if workflow.agent.time_limit_minutes.is_some() { "workflow" } else { "settings" },
                },
                "editable": editable(state)?,
            }))
        }
        "workflow_blocks_list" => {
            ensure_no_arguments(args)?;
            let map = state.settings.map().map_err(fail)?;
            let used_by: BTreeMap<&str, Vec<&str>> = map
                .workflow_blocks
                .iter()
                .map(|b| (b.id.as_str(), core::using(&map.workflows, &b.id)))
                .collect();
            Ok(json!({"blocks":map.workflow_blocks,"used_by":used_by,"editable":editable(state)?}))
        }
        "workflow_repositories" => {
            let a: RepositoriesArgs = parse(args)?;
            if a.project.trim().is_empty() || a.project.len() > 120 {
                return Err(invalid("Give the Azure DevOps project's name."));
            }
            tools::todo::repositories(
                state,
                a.source.as_deref(),
                a.organization.as_deref(),
                &a.project,
            )
            .await
        }
        "workflow_save" => save(host, parse(args)?).await,
        "workflow_remove" => {
            let a: RemoveArgs = parse(args)?;
            require_coordinator(state)?;
            if !a.confirmed {
                return Err(invalid(format!(
                    "Confirm removing workflow {}: its definition cannot be recovered.",
                    a.id
                )));
            }
            if runs::has_active(state, &a.id).map_err(fail)? {
                return Err(conflict(format!(
                    "Workflow {} has a run that is not done or cancelled; let it finish or cancel it first.",
                    a.id
                )));
            }
            change(host, |map| {
                let before = map.workflows.len();
                map.workflows.retain(|w| w.id != a.id);
                if map.workflows.len() == before {
                    return Err(invalid(format!("There is no workflow {}.", a.id)));
                }
                Ok(())
            })
            .await?;
            Ok(json!({"removed":a.id}))
        }
        "workflow_leads_list" => {
            let map = state.settings.map().map_err(fail)?;
            let leads: Vec<Value> = map
                .workflow_leads
                .iter()
                .map(|l| json!({"lead": l, "workflows": lead::led_by(&map.workflows, &l.id)}))
                .collect();
            Ok(json!({"leads": leads, "editable": editable(state)?}))
        }
        "workflow_lead_show" => {
            let a: LeadArgs = parse(args)?;
            let map = state.settings.map().map_err(fail)?;
            let found = lead::find(&map.workflow_leads, &a.lead)
                .ok_or_else(|| invalid(format!("There is no workflow lead {}.", a.lead)))?;
            Ok(
                json!({"lead": found, "workflows": lead::led_by(&map.workflows, &found.id), "editable": editable(state)?}),
            )
        }
        "workflow_lead_save" => save_lead(host, parse(args)?).await,
        "workflow_lead_shifts" | "workflow_lead_wake" | "workflow_lead_answer" => {
            shifts::operation(host, method, args)
        }
        "workflow_lead_connect" | "workflow_lead_disconnect" => {
            shifts::talk(host, method, args, &engine::Azure::Live).await
        }
        "workflow_lead_remove" => {
            let a: LeadArgs = parse(args)?;
            require_coordinator(state)?;
            if !a.confirmed {
                let map = state.settings.map().map_err(fail)?;
                let named = lead::find(&map.workflow_leads, &a.lead)
                    .map_or_else(|| a.lead.clone(), |l| format!("{} ({})", l.name, l.id));
                return Err(invalid(format!(
                    "Confirm with the user, then remove with confirmed: true. Remove workflow lead {named}? Its definition cannot be recovered."
                )));
            }
            let removed = change(host, |map| {
                let found = lead::find(&map.workflow_leads, &a.lead)
                    .ok_or_else(|| invalid(format!("There is no workflow lead {}.", a.lead)))?
                    .id
                    .clone();
                let users = lead::led_by(&map.workflows, &found);
                if !users.is_empty() {
                    return Err(conflict(format!(
                        "Lead {} leads workflow {}; take it out of them first.",
                        a.lead,
                        users.join(", ")
                    )));
                }
                map.workflow_leads.retain(|l| l.id != found);
                Ok(found)
            })
            .await?;
            Ok(json!({"removed": removed}))
        }
        "workflow_block_save" => save_block(host, parse(args)?).await,
        "workflow_block_remove" => {
            let a: IdArgs = parse(args)?;
            require_coordinator(state)?;
            change(host, |map| {
                let users = core::using(&map.workflows, &a.id);
                if !users.is_empty() {
                    return Err(conflict(format!(
                        "Block {} is used by workflow {}; take it out of them first.",
                        a.id,
                        users.join(", ")
                    )));
                }
                let before = map.workflow_blocks.len();
                map.workflow_blocks.retain(|b| b.id != a.id);
                if map.workflow_blocks.len() == before {
                    return Err(invalid(format!("There is no workflow block {}.", a.id)));
                }
                Ok(())
            })
            .await?;
            Ok(json!({"removed":a.id}))
        }
        _ => Err(invalid("Unknown workflow operation.")),
    }
}

fn ensure_no_arguments(args: &Value) -> Result<(), String> {
    if args.as_object().is_some_and(|a| !a.is_empty()) {
        return Err(invalid("This operation takes no arguments."));
    }
    Ok(())
}

/// Change the map's workflows or blocks and commit it with the settings commit, under the
/// operations lock, from the map as it is now.
async fn change<T>(
    host: &Arc<Host>,
    edit: impl FnOnce(&mut KnowledgeMap) -> Result<T, String>,
) -> Result<T, String> {
    let state = &host.state;
    let _guard = state.operations.lock().await;
    if activity::active(state).await {
        return Err(conflict(
            "Activity registration is running. Wait for it to finish before changing workflows.",
        ));
    }
    let config = state.settings.config().map_err(fail)?;
    let mut map = state.settings.map().map_err(fail)?;
    let tunnel = state.settings.tunnel().map_err(fail)?;
    let out = edit(&mut map)?;
    core::validate(&map.workflows, &map.workflow_blocks).map_err(invalid)?;
    commit_settings(host, config, map, tunnel).await?;
    Ok(out)
}

async fn save(host: &Arc<Host>, args: SaveArgs) -> Result<Value, String> {
    let state = &host.state;
    require_coordinator(state)?;
    match (args.workflow, args.id, args.enabled) {
        // Enabling or disabling changes nothing else (what it allows was confirmed when saved).
        (None, Some(id), Some(enabled)) => {
            let workflow = change(host, |map| {
                let workflow = map
                    .workflows
                    .iter_mut()
                    .find(|w| w.id == id)
                    .ok_or_else(|| invalid(format!("There is no workflow {id}.")))?;
                workflow.enabled = enabled;
                Ok(workflow.clone())
            })
            .await?;
            Ok(json!({"workflow":shown(&workflow),"saved":true,"warnings":[]}))
        }
        (Some(value), None, None) => {
            let mut workflow: Workflow = serde_json::from_value(value)
                .map_err(|e| invalid(format!("Invalid workflow: {e}.")))?;
            workflow.updated_at = now();
            // Refuses checkouts keyed twice by one repository, then keys them in lowercase.
            core::validate_workflow(&workflow).map_err(invalid)?;
            workflow.allow.sort();
            workflow.preapprove.sort_by_key(|p| p.action);
            // A pre-approved post's chat is one of the user's, under the name their chat list
            // gives it, before the confirmation shows it.
            preapproved_chats(state, &workflow).await?;
            // Lifting a fixed rule of its coding agents is the user's decision, confirmed with
            // what it lets them do.
            if let Some(question) = describe_workflow(&workflow)
                && !args.confirmed
            {
                return Err(invalid(format!(
                    "Confirm with the user, then save with confirmed: true. {question}"
                )));
            }
            workflow.checkouts = std::mem::take(&mut workflow.checkouts)
                .into_iter()
                .map(|(id, directory)| (id.to_lowercase(), directory))
                .collect();
            // Everything the map alone can refuse, before any machine or catalog is read.
            let mut candidate = state.settings.map().map_err(fail)?;
            upsert(&mut candidate.workflows, workflow.clone(), |w| &w.id);
            core::validate(&candidate.workflows, &candidate.workflow_blocks).map_err(invalid)?;
            lead::validate(&candidate.workflow_leads, &candidate.workflows).map_err(invalid)?;
            fits(
                &workflow,
                &candidate.workflow_blocks,
                &Texts::new(&candidate.agent_prompts),
            )?;
            let current = state.settings.map().map_err(fail)?;
            if let Some(lead) = &workflow.lead {
                lead_fits(host, &candidate, &lead.id, Some(&current))?;
            }
            let mut warnings = verify(host, &mut workflow).await?;
            // A pre-approved post goes only to a chat the user belongs to.
            let saved = workflow.clone();
            // Again with the directories as the machine spells them and the blocks as they are now.
            let used = change(host, |map| {
                let texts = Texts::new(&map.agent_prompts);
                fits(&workflow, &map.workflow_blocks, &texts)?;
                let used = usage(&workflow, &map.workflow_blocks, &texts);
                let before = map.clone();
                let lead = workflow.lead.clone();
                upsert(&mut map.workflows, workflow, |w| &w.id);
                if let Some(lead) = lead {
                    lead_fits(host, map, &lead.id, Some(&before))?;
                }
                Ok(used)
            })
            .await?;
            warnings.extend(older_hosts(host, &saved).await);
            warnings.extend(older_supervisors(host, &saved).await);
            warnings.extend(older_delivery(host, &saved).await);
            if saved.lead.is_some() || !saved.done_when.is_empty() {
                warnings.extend(
                    older_leads(
                        host,
                        &format!("Workflow {} names a lead or completion criteria", saved.id),
                    )
                    .await,
                );
            }
            warnings.extend(older_time_limit(host, &saved).await);
            Ok(json!({"workflow":shown(&saved),"saved":true,"usage":used,"warnings":warnings}))
        }
        _ => Err(invalid(
            "Give the workflow to save, or its id and enabled to enable or disable it.",
        )),
    }
}

fn upsert<T>(items: &mut Vec<T>, item: T, id: impl Fn(&T) -> &String) {
    match items.iter().position(|i| id(i) == id(&item)) {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
}

/// How long reading the project's catalog may take while a workflow is saved: with the one
/// round of machine calls after it (at most 40 seconds, run together), the save answers within
/// the control channel's 120-second client deadline, so nothing is committed after a client
/// gave up waiting.
const CATALOG_SECONDS: u64 = 60;

/// Gather what the workflow is checked against and check it; returns the warnings.
async fn verify(host: &Arc<Host>, workflow: &mut Workflow) -> Result<Vec<String>, String> {
    let state = &host.state;
    let machine = implementation::machine_facts(host, &workflow.machine)
        .await
        .map_err(fail)?
        .ok_or_else(|| {
            invalid(format!(
                "Machine {} is not an execution machine of this account (machine_list).",
                workflow.machine
            ))
        })?;
    if machine.own {
        // The shared server runs no coding agent for another account.
        authority::require_execution(state).map_err(|_| invalid(authority::EXECUTION_DENIED))?;
    }
    let listed = tokio::time::timeout(
        std::time::Duration::from_secs(CATALOG_SECONDS),
        tools::todo::repositories(state, None, Some(&workflow.organization), &workflow.project),
    )
    .await
    .map_err(|_| {
        "[network] The project's Git repositories could not be read in time.".to_owned()
    })?;
    let mut facts = Facts {
        project_known: false,
        catalog: Vec::new(),
        os: machine.os.clone(),
        roots: machine.roots.clone(),
        agents: Err("not asked".into()),
        checkouts: BTreeMap::new(),
    };
    match listed {
        Ok(listed) => {
            // The project's own spelling.
            if let Some(project) = listed["project"].as_str() {
                workflow.project = project.to_owned();
            }
            facts.catalog = serde_json::from_value(listed["repositories"].clone()).map_err(fail)?;
            facts.project_known = true;
        }
        // Refused at once, without asking the machine anything.
        Err(error) if error.starts_with("[invalid_input] invalid project") => {
            return core::check(workflow, &facts).map_err(invalid);
        }
        Err(error) => return Err(error),
    }
    let directories: Vec<(String, PathBuf)> = workflow
        .repositories
        .iter()
        .filter_map(|r| {
            let key = r.id.to_lowercase();
            workflow.checkouts.get(&key).map(|d| (key, d.clone()))
        })
        .collect();
    let paths: Vec<PathBuf> = directories.iter().map(|(_, d)| d.clone()).collect();
    let agents = async {
        if machine.own || machine.online {
            implementation::machine_call(
                host,
                &workflow.machine,
                "implementation_catalog",
                json!({}),
            )
            .await
            .map_err(|_| "the machine did not answer".to_owned())
        } else {
            Err("the machine is offline".to_owned())
        }
    };
    let (agents, inspections) =
        tokio::join!(agents, inspect(host, &workflow.machine, &machine, &paths));
    facts.agents = agents;
    facts.checkouts = directories
        .into_iter()
        .map(|(key, _)| key)
        .zip(inspections)
        .collect();
    core::check(workflow, &facts).map_err(invalid)
}

/// Every checkout as the machine sees it, asked in one `checkout_inspect` call, or unknown when
/// the machine is offline, its version predates the call (it would not answer it) or its
/// answer cannot be read.
async fn inspect(
    host: &Arc<Host>,
    id: &str,
    machine: &implementation::MachineFacts,
    directories: &[PathBuf],
) -> Vec<Inspection> {
    let unknown = |reason: &str| vec![Inspection::Unknown(reason.into()); directories.len()];
    if !(machine.own || machine.online) {
        return unknown("the machine is offline");
    }
    if !machine.inspect {
        return unknown("the machine's version cannot inspect checkouts; update it");
    }
    let answer = match implementation::machine_call(
        host,
        id,
        "checkout_inspect",
        json!({"paths":directories}),
    )
    .await
    {
        Ok(answer) => answer,
        Err(_) => return unknown("the machine did not answer"),
    };
    match answer["checkouts"].as_array() {
        Some(answers) if answers.len() == directories.len() => answers
            .iter()
            .map(implementation::checkout::inspection)
            .collect(),
        _ => unknown("the machine's answer could not be read"),
    }
}

/// The personal chat (Teams notes) is always the user's own.
const PERSONAL_CHAT: &str = "48:notes";
/// How long reading the user's chats may take while a block is saved.
const CHATS_SECONDS: u64 = 60;

/// Whether the user belongs to the chat, from the same chat list `kuroko teams chats` shows: a
/// refusal when the list says no, a warning when it cannot be read here (no Microsoft session
/// on this computer, or Graph did not answer). Mentions are resolved when a notification is
/// posted, against that chat's members.
async fn verify_chat(state: &DesktopState, chat: &str) -> Result<Option<String>, String> {
    if chat == PERSONAL_CHAT {
        return Ok(None);
    }
    let read = tokio::time::timeout(std::time::Duration::from_secs(CHATS_SECONDS), async {
        let graph = teams_graph(state).await?;
        graph.chat_directory(None).await
    })
    .await;
    match read {
        Ok(Ok(chats)) if chats.iter().any(|c| c["id"] == chat) => Ok(None),
        Ok(Ok(_)) => Err(invalid(format!(
            "You are not a member of the chat {chat}; choose one that kuroko teams chats lists."
        ))),
        _ => Ok(Some(format!(
            "The chat {chat} could not be checked against your chats (no Microsoft session could read them here); it is checked again when the block posts."
        ))),
    }
}

/// A pre-approved post goes, with nobody reading it first, only to a chat the user belongs to:
/// the chat must be in the user's chat list, read now, under the name that list gives it
/// (otherwise the confirmation would show the caller's label). A list that cannot be read
/// refuses the save: a destination nobody could check is not pre-approved.
pub(super) async fn preapproved_chats(
    state: &DesktopState,
    workflow: &Workflow,
) -> Result<(), String> {
    let chats: Vec<&core::Chat> = workflow
        .preapprove
        .iter()
        .filter_map(|p| p.chat.as_ref())
        .collect();
    if chats.is_empty() {
        return Ok(());
    }
    let read = tokio::time::timeout(std::time::Duration::from_secs(CHATS_SECONDS), async {
        let graph = teams_graph(state).await?;
        graph.chat_directory(None).await
    })
    .await;
    let Ok(Ok(directory)) = read else {
        return Err("[not_ready] Your chats could not be read here (no Microsoft session could read them): a pre-approved post needs its chat checked against your chats. Save it where they can be read.".into());
    };
    for chat in chats {
        let Some(listed) = directory.iter().find(|c| c["id"] == chat.id.as_str()) else {
            return Err(invalid(format!(
                "You are not a member of the chat {}; choose one that kuroko teams chats lists.",
                chat.id
            )));
        };
        let name = listed["topic"].as_str().unwrap_or_default();
        if name != chat.name {
            return Err(invalid(format!(
                "The chat {} is named «{name}» in your chats; save the pre-approved post with that name.",
                chat.id
            )));
        }
    }
    Ok(())
}

/// Save a workflow lead: its own rules, its agent installed on this coordinator (what cannot be
/// read now is a warning), and a name no other lead has.
async fn save_lead(host: &Arc<Host>, args: LeadSaveArgs) -> Result<Value, String> {
    let state = &host.state;
    require_coordinator(state)?;
    let mut saved: Lead =
        serde_json::from_value(args.lead).map_err(|e| invalid(format!("Invalid lead: {e}.")))?;
    saved.updated_at = now();
    lead::validate_lead(&saved).map_err(invalid)?;
    let director = core::director::Director {
        machine: implementation::own_machine(host).await,
        provider: saved.agent.provider.clone(),
        model: saved.agent.model.clone(),
        effort: saved.agent.effort.clone(),
    };
    let mut warnings = director::verify_agent(host, &director, director::Role::Lead).await?;
    warnings.extend(older_leads(host, "The account now has workflow leads").await);
    let kept = saved.clone();
    change(host, |map| {
        let id = kept.id.clone();
        let before = map.clone();
        upsert(&mut map.workflow_leads, kept, |l| &l.id);
        lead::validate(&map.workflow_leads, &map.workflows).map_err(invalid)?;
        lead_fits(host, map, &id, Some(&before))
    })
    .await?;
    Ok(json!({"lead": saved, "saved": true, "warnings": warnings}))
}

async fn save_block(host: &Arc<Host>, args: BlockSaveArgs) -> Result<Value, String> {
    let state = &host.state;
    require_coordinator(state)?;
    let mut block: Block =
        serde_json::from_value(args.block).map_err(|e| invalid(format!("Invalid block: {e}.")))?;
    block.updated_at = now();
    core::validate_block(&block).map_err(invalid)?;
    let mut warnings = Vec::new();
    // A block that acts is confirmed with what it does, on every surface.
    if block.kind != Kind::Instruction && !args.confirmed {
        return Err(invalid(format!(
            "Confirm this block with the user, then save it with confirmed: {}.",
            describe(&block)
        )));
    }
    match block.kind {
        Kind::Instruction => {}
        Kind::TeamsNotification => {
            let chat = block.chat.clone().unwrap_or_default();
            warnings.extend(verify_chat(state, &chat.id).await?);
        }
        Kind::Launcher => {
            let invocation = authority::invocation(state)
                .map_err(|_| conflict(authority::ACCOUNT_UNAVAILABLE))?;
            if !(invocation.scope.owner
                && matches!(
                    invocation.transport,
                    authority::Transport::Window | authority::Transport::Browser
                ))
            {
                return Err(conflict(
                    "Only the owner saves a launcher block, in this computer's application (the local page kuroko app open shows) or the owner's own portal.",
                ));
            }
            let reference = block.launcher.clone().unwrap_or_default();
            match consumers::launcher_approval(state, &reference.id) {
                None => {
                    return Err(invalid(format!(
                        "There is no launcher {} on the coordinator.",
                        reference.id
                    )));
                }
                Some((_, false)) => {
                    return Err(conflict(format!(
                        "Launcher {} is not approved yet; approve it first.",
                        reference.id
                    )));
                }
                Some((digest, true)) if digest != reference.digest => {
                    return Err(conflict(format!(
                        "Launcher {} is approved with another program digest; review it again.",
                        reference.id
                    )));
                }
                Some(_) => {}
            }
        }
    }
    let saved = block.clone();
    change(host, |map| {
        upsert(&mut map.workflow_blocks, block, |b| &b.id);
        // A longer text is part of every workflow that lists the block.
        for workflow in map
            .workflows
            .iter()
            .filter(|w| w.blocks.contains(&saved.id))
        {
            fits(
                workflow,
                &map.workflow_blocks,
                &Texts::new(&map.agent_prompts),
            )?;
        }
        Ok(())
    })
    .await?;
    let mut reply = json!({"block":saved,"saved":true,"warnings":warnings});
    if saved.kind != Kind::Instruction {
        reply["confirmation"] = json!(describe(&saved));
    }
    if saved.kind == Kind::TeamsNotification {
        reply["chat"] = json!(saved.chat);
        reply["mentions"] = json!(saved.mentions);
        reply["template"] = json!(saved.template);
    }
    Ok(reply)
}

#[cfg(test)]
mod tests;
