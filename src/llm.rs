use crate::config::{Language, Llm, LlmChoice};
use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) mod agent;
mod deepseek;
pub(crate) mod implementation;
pub mod prompts;
pub use agent::{Catalog, ClaudeCli, CodexCli, OpenCodeCli, api_catalog, catalog};
pub(crate) use agent::{child_env, locate};
pub use deepseek::DeepSeek;

#[derive(Clone, Copy, Default, Serialize)]
pub struct GenerationInput<'a> {
    pub question: &'a str,
    pub evidence: &'a str,
    pub detail_requested: bool,
    /// Earlier messages of the conversation with author and time. Context, never evidence.
    #[serde(rename = "conversation_history", skip_serializing_if = "str::is_empty")]
    pub history: &'a str,
    /// The evidence compares the user's activity with registered work (an activity review).
    #[serde(skip)]
    pub review: bool,
    /// The app adds where to register each piece of work and asks to confirm it.
    #[serde(skip)]
    pub proposing: bool,
    /// The message asks for no answer (informational, or a request for the person) and the
    /// user asked the app to write one anyway.
    #[serde(skip)]
    pub unprompted: bool,
    /// The evidence is the user's own working sessions with coding agents, not documentation:
    /// the answer says the work is in progress and what it found so far.
    #[serde(skip)]
    pub in_progress: bool,
    /// Context and corrections the user gave to improve an answer that was not sent.
    #[serde(rename = "owner_guidance", skip_serializing_if = "str::is_empty")]
    pub guidance: &'a str,
    /// The answer that was not sent, which the user asked to improve.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub previous_draft: &'a str,
    /// The user's instructions for the subject the request is about (a context in the map).
    #[serde(skip_serializing_if = "str::is_empty")]
    pub context_instructions: &'a str,
    /// The user's own texts for the cases (empty = the defaults in `prompts`).
    #[serde(skip)]
    pub prompts: Prompts<'a>,
    /// The user's note about the person who wrote: how to address them, what to keep in mind.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub recipient: &'a str,
    /// Earlier answers the user approved for similar requests, for structure and tone.
    #[serde(rename = "approved_examples", skip_serializing_if = "str::is_empty")]
    pub examples: &'a str,
    /// Owner ratings retrieved for this question, including corrections to avoid.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub evaluations: &'a str,
}
/// The user's texts for each case's part of the generation prompt; an empty one keeps the
/// default in `prompts`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Prompts<'a> {
    pub general: &'a str,
    pub status_round: &'a str,
    pub recap: &'a str,
    pub documentation: &'a str,
    pub activity_review: &'a str,
    pub statement: &'a str,
    /// The user configured questions: the status round and the recap follow
    /// `answers.questions_status_round` and `answers.questions_recap` unless set here.
    pub questions: bool,
}
/// The user's descriptions of the intent categories; an empty one keeps the default.
#[derive(Clone, Copy, Debug, Default)]
pub struct Categories<'a> {
    pub question: &'a str,
    pub activity_review: &'a str,
    pub personal: &'a str,
    pub statement: &'a str,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedAnswer {
    pub answer: String,
    pub detailed: bool,
    #[serde(default)]
    pub used_sources: Vec<String>,
    /// `provider:model:effort` that wrote the answer; set by code, never by the model.
    #[serde(skip)]
    pub provider: Option<String>,
    /// Providers that failed before `provider` answered, as `label: failure`.
    #[serde(skip)]
    pub fallbacks: Vec<String>,
}
/// A follow-up rewritten so retrieval does not depend on the conversation history.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StandaloneRequest {
    /// The current request with implicit references to the previous one resolved.
    pub question: String,
    /// Component, procedure or document to search for, as it would appear in a page title.
    pub topic: String,
}
/// What a message asks of the assistant. Code decides the clear cases; a model the rest.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Asks for information, an explanation or an answer.
    Question,
    /// Asks to compare the user's own activity with the work registered as tasks.
    ActivityReview,
    /// Asks the person for their time or their own action: a call, a meeting, availability.
    Personal,
    Greeting,
    /// Informs or instructs without asking for an answer.
    Statement,
}
impl Intent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::ActivityReview => "activity_review",
            Self::Personal => "personal",
            Self::Greeting => "greeting",
            Self::Statement => "statement",
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentDecision {
    pub intent: Intent,
    pub confidence: f64,
}
impl IntentDecision {
    /// A decision below the threshold (or with an invalid confidence) is not acted on.
    pub fn confident(&self) -> bool {
        self.confidence.is_finite() && (0.5..=1.).contains(&self.confidence)
    }
}
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    /// Resolve a follow-up against the previous exchange and the earlier messages of the
    /// conversation (`history`, with author and time). `None` keeps the literal request.
    async fn standalone_request(
        &self,
        _previous_question: &str,
        _previous_answer: &str,
        _history: &str,
        _current: &str,
    ) -> Result<Option<StandaloneRequest>> {
        Ok(None)
    }
    async fn select_tool(
        &self,
        _: &str,
        _: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        Ok(None)
    }
    /// Classify a message the code could not classify. It never selects sources: audiences,
    /// tools and limits are decided by code.
    /// `classify_intent` with the user's own descriptions of the categories (empty = default).
    async fn classify_intent_as(
        &self,
        message: &str,
        history: &str,
        _categories: Categories<'_>,
    ) -> Result<IntentDecision> {
        self.classify_intent(message, history).await
    }
    /// Identify an equivalent configured question using only the supplied IDs.
    async fn identify_question(
        &self,
        _request: &str,
        _history: &str,
        _questions: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        Ok(None)
    }
    /// The context (closed IDs, each with its name and description) a request is about, if
    /// any. It only guides: a failure or `None` reads as no context.
    async fn select_context(
        &self,
        _question: &str,
        _contexts: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        Ok(None)
    }
    /// The user's working sessions (closed keys, each with its summary) that are about what
    /// the current request, read with the conversation's earlier messages, refers to. It only
    /// narrows evidence code already authorized; an empty answer leaves the message as it was.
    async fn select_sessions(
        &self,
        _request: &str,
        _history: &str,
        _sessions: &std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn classify_intent(&self, _message: &str, _history: &str) -> Result<IntentDecision> {
        bail!("intent classification unavailable")
    }
    /// The verified `references` the drafted answer uses, each with the words of the answer
    /// that name it, which code turns into its link. Only IDs from `references` are returned;
    /// any other answer is an error and the code links what it sees named.
    async fn select_references(
        &self,
        _answer: &str,
        _references: &[crate::evidence::Reference],
    ) -> Result<Vec<crate::evidence::Used>> {
        bail!("reference selection unavailable")
    }
    /// Turn the user's reply to an activity review into the links it asks for, as the offer's
    /// closed keys and work item IDs. Code validates every key and ID and the user confirms
    /// the plan before anything is written; an empty plan leaves the reply to the pipeline.
    async fn plan_links(
        &self,
        _reply: &str,
        _offer: &crate::ado::link::LinkOffer,
    ) -> Result<Vec<crate::ado::link::PlannedLink>> {
        Ok(Vec::new())
    }
    /// Propose where to register each unlinked activity of a review: an existing work item,
    /// or a new task under one of the user's HUs, with a short reason in `language`. Code
    /// validates every key and ID and shows the plan for confirmation.
    async fn propose_registration(
        &self,
        _offer: &crate::ado::link::LinkOffer,
        _language: Language,
    ) -> Result<Vec<crate::ado::link::Proposal>> {
        bail!("registration proposals unavailable")
    }
    /// Plan completed tasks from closed activity keys and verified HUs or bugs that are not closed,
    /// with a description and completion evidence that never reveal chat content.
    async fn plan_activity_tasks(
        &self,
        _activities: &[crate::ado::link::Linkable],
        _parents: &[crate::ado::link::Target],
        _evidence: &str,
        _remaining: f64,
        _context: &str,
    ) -> Result<Vec<crate::activity::Draft>> {
        bail!("activity task planning unavailable")
    }
    /// `text`, a Teams message, rewritten in at most `max_chars` characters with the same
    /// meaning, links and mentions. Code checks the result (`card::condense`) and measures it
    /// again; nothing here sends anything.
    async fn condense(&self, _text: &str, _max_chars: usize) -> Result<String> {
        bail!("message condensing unavailable")
    }
    async fn generate(&self, input: GenerationInput<'_>) -> Result<String>;
    async fn generate_response(&self, input: GenerationInput<'_>) -> Result<GeneratedAnswer> {
        let detailed = input.detail_requested;
        Ok(GeneratedAnswer {
            answer: self.generate(input).await?,
            detailed,
            used_sources: Vec::new(),
            provider: None,
            fallbacks: Vec::new(),
        })
    }
}

/// Providers in the order the GUI offers them. CLIs use their own login; DeepSeek an API key.
pub const PROVIDERS: [&str; 4] = ["codex", "claude", "opencode", "deepseek"];
pub const MAX_CHAIN_LENGTH: usize = 16;

/// Reasoning efforts each provider accepts. Codex `ultra` delegates to sub-agents, so it is
/// never offered: the answer must come from one tool-less model call.
pub fn efforts(provider: &str) -> &'static [&'static str] {
    match provider {
        "codex" => &["minimal", "low", "medium", "high", "xhigh", "max"],
        "claude" => &["low", "medium", "high", "xhigh", "max"],
        "deepseek" => &["none", "low", "high", "max"],
        // Named variants come from each OpenCode model's catalog.
        "opencode" => &["default"],
        _ => &[],
    }
}

pub fn validate_provider(name: &str) -> Result<()> {
    if PROVIDERS.contains(&name) {
        Ok(())
    } else {
        bail!("unsupported LLM provider; register an LlmProvider implementation")
    }
}

/// Model IDs reach CLI arguments: a closed character set and no leading dash.
pub(crate) fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 100
        && model
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/[]-".contains(c))
}

/// A variant is appended after `#` inside one model argument; never allow another reference.
fn valid_variant(variant: &str) -> bool {
    !variant.is_empty()
        && variant.len() <= 100
        && variant.starts_with(|c: char| c.is_ascii_alphanumeric())
        && variant
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

pub fn validate(llm: &Llm) -> Result<()> {
    validate_provider(&llm.provider)?;
    ensure!(valid_model(&llm.model), "invalid LLM model");
    ensure!(llm.chain.len() <= MAX_CHAIN_LENGTH, "too many LLM models");
    for choice in &llm.chain {
        validate_provider(&choice.provider)?;
        ensure!(valid_model(&choice.model), "invalid LLM model");
        if choice.provider == "opencode" {
            ensure!(
                choice
                    .model
                    .split_once('/')
                    .is_some_and(|(p, m)| !p.is_empty() && !m.is_empty()),
                "OpenCode model must use provider/model"
            );
            ensure!(valid_variant(&choice.effort), "invalid OpenCode variant");
        } else {
            ensure!(
                efforts(&choice.provider).contains(&choice.effort.as_str()),
                "unsupported reasoning effort for this LLM provider"
            );
        }
    }
    ensure!(!llm.active().is_empty(), "enable at least one LLM provider");
    Ok(())
}

/// Why a provider did not answer. Errors carry only this class, never provider output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Out of credits, usage or rate limit: the next provider answers this call.
    UsageLimit,
    NotInstalled,
    /// Answered, but not with the requested JSON contract.
    InvalidAnswer,
    Failed,
}
impl Failure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UsageLimit => "usage_limit",
            Self::NotInstalled => "not_installed",
            Self::InvalidAnswer => "invalid_answer",
            Self::Failed => "failed",
        }
    }
}
#[derive(Debug)]
pub struct Unavailable(pub Failure);
impl std::fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "LLM provider unavailable: {}", self.0.as_str())
    }
}
impl std::error::Error for Unavailable {}

/// Classify provider error text; only the class leaves this function.
fn usage_limited(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "usage limit",
        "usage_limit",
        "rate limit",
        "rate_limit",
        "limit reached",
        "hit your",
        "usage credits",
        "credit balance",
        "insufficient",
        "quota",
        "billing",
        "too many requests",
        "\"status\":429",
        "\"status\":402",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}
fn failure_of(error: &anyhow::Error) -> Failure {
    failure_class(error).unwrap_or(Failure::Failed)
}
/// The provider failure behind an error, if a provider (not a later check) failed.
pub fn failure_class(error: &anyhow::Error) -> Option<Failure> {
    error.downcast_ref::<Unavailable>().map(|e| e.0)
}

/// One JSON completion from a model transport (HTTP API or a local CLI). Prompts, contracts
/// and validation live in [`Model`], so every provider answers under the same rules.
#[async_trait]
pub trait Backend: Send + Sync {
    /// `provider:model:effort`, for audit and logs.
    fn label(&self) -> &str;
    /// Answer `user` under `system` with one JSON object shaped by `schema`.
    async fn complete_json(&self, system: &str, user: &str, schema: &Value) -> Result<String>;
}

fn select_schema() -> Value {
    json!({"type":"object","properties":{"source":{"type":["string","null"]}},"required":["source"],"additionalProperties":false})
}
fn context_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["context"],"properties":{"context":{"type":["string","null"]}}})
}
fn sessions_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["sessions"],"properties":{"sessions":{"type":"array","items":{"type":"string"}}}})
}
fn standalone_schema() -> Value {
    json!({"type":"object","properties":{"question":{"type":"string"},"topic":{"type":"string"}},"required":["question","topic"],"additionalProperties":false})
}
fn condensed_schema() -> Value {
    json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false})
}
fn answer_schema() -> Value {
    json!({"type":"object","properties":{"answer":{"type":"string"},"detailed":{"type":"boolean"}},"required":["answer","detailed"],"additionalProperties":false})
}
fn intent_schema() -> Value {
    json!({"type":"object","properties":{"intent":{"type":"string","enum":["question","activity_review","personal","greeting","statement"]},"confidence":{"type":"number"}},"required":["intent","confidence"],"additionalProperties":false})
}
fn proposals_schema() -> Value {
    json!({"type":"object","properties":{"proposals":{"type":"array","items":{"type":"object","properties":{"activity":{"type":"string"},"action":{"type":"string","enum":["link","create"]},"work_item":{"type":["integer","null"]},"parent":{"type":["integer","null"]},"title":{"type":["string","null"]},"reason":{"type":"string"}},"required":["activity","action","work_item","parent","title","reason"],"additionalProperties":false}}},"required":["proposals"],"additionalProperties":false})
}
fn links_schema() -> Value {
    json!({"type":"object","properties":{"links":{"type":"array","items":{"type":"object","properties":{"activity":{"type":"string"},"work_item":{"type":"integer"}},"required":["activity","work_item"],"additionalProperties":false}}},"required":["links"],"additionalProperties":false})
}
fn activity_schema() -> Value {
    json!({"type":"object","properties":{"tasks":{"type":"array","maxItems":40,"items":{"type":"object","properties":{
        "activities":{"type":"array","items":{"type":"string"}},"parent":{"type":["integer","null"]},"title":{"type":"string"},"description":{"type":"string"},"evidence":{"type":"string"},"hours":{"type":["number","null"]},"confidence":{"type":"number"},"question":{"type":"string"},"suggested_parents":{"type":"array","items":{"type":"integer"}},"ignore":{"type":"boolean"}
    },"required":["activities","parent","title","description","evidence","hours","confidence","question","suggested_parents","ignore"],"additionalProperties":false}}},"required":["tasks"],"additionalProperties":false})
}
fn references_schema() -> Value {
    json!({"type":"object","properties":{"used":{"type":"array","items":{"type":"object","properties":{"key":{"type":"string"},"mention":{"type":"string"}},"required":["key","mention"],"additionalProperties":false}}},"required":["used"],"additionalProperties":false})
}
/// Parse a model's JSON object; a broken contract is the provider's failure, so the next
/// provider in the chain answers the call.
fn contract<T: serde::de::DeserializeOwned>(response: &str) -> Result<T> {
    serde_json::from_str(response).map_err(|_| Unavailable(Failure::InvalidAnswer).into())
}

/// A model transport behind the shared prompts and answer contracts.
pub struct Model {
    backend: Box<dyn Backend>,
    style: String,
    language: Language,
    /// The prompts in effect (`crate::agent_prompts`): the user's changes over the defaults.
    texts: std::sync::Arc<crate::agent_prompts::Texts>,
}
impl Model {
    pub fn new(backend: impl Backend + 'static, style: &str) -> Self {
        Self {
            backend: Box::new(backend),
            style: style.into(),
            language: Language::default(),
            texts: Default::default(),
        }
    }
    /// Language the answers are written in (Spanish unless set).
    pub fn language(mut self, language: Language) -> Self {
        self.language = language;
        self
    }
    /// The user's prompts (the defaults unless set).
    pub fn texts(mut self, texts: std::sync::Arc<crate::agent_prompts::Texts>) -> Self {
        self.texts = texts;
        self
    }
}
#[async_trait]
impl LlmProvider for Model {
    async fn plan_activity_tasks(
        &self,
        activities: &[crate::ado::link::Linkable],
        parents: &[crate::ado::link::Target],
        evidence: &str,
        remaining: f64,
        context: &str,
    ) -> Result<Vec<crate::activity::Draft>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Plans {
            tasks: Vec<crate::activity::Draft>,
        }
        let activities:Vec<_>=activities.iter().map(|a|json!({"key":a.key,"date":a.date,"activity":a.label,"needs_clarification":a.context_required,"duration_hours":a.duration_hours,"occurred_at":a.occurred_at})).collect();
        let parents: Vec<_> = parents
            .iter()
            .map(|p| json!({"id":p.id,"title":p.title,"project":p.project}))
            .collect();
        let language = match self.language {
            Language::Es => "Spanish",
            Language::En => "English",
        };
        let response=self.backend.complete_json(&self.texts.render("activity.plan_tasks", &[("language", language)]),&json!({"activities":activities,"parents":parents,"evidence":evidence,"remaining_hours":remaining,"user_context":context}).to_string(),&activity_schema()).await?;
        let plans: Plans = contract(&response)?;
        if plans.tasks.len() > 40
            || plans.tasks.iter().any(|d| {
                !d.confidence.is_finite()
                    || !(0.0..=1.0).contains(&d.confidence)
                    || d.hours
                        .is_some_and(|h| !h.is_finite() || h <= 0. || h > 24.)
            })
        {
            return Err(Unavailable(Failure::InvalidAnswer).into());
        }
        Ok(plans.tasks)
    }
    fn name(&self) -> &str {
        self.backend.label()
    }
    async fn select_tool(
        &self,
        question: &str,
        candidates: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Selection {
            source: Option<String>,
        }
        let response = self
            .backend
            .complete_json(
                &self.texts.get("steps.select_source"),
                &serde_json::json!({"question":question,"sources":candidates}).to_string(),
                &select_schema(),
            )
            .await
            .context("tool selection failed")?;
        let selected: Selection = contract(&response)?;
        anyhow::ensure!(
            selected
                .source
                .as_ref()
                .is_none_or(|id| candidates.contains_key(id)),
            "tool selection outside authorized sources"
        );
        Ok(selected.source)
    }
    async fn identify_question(
        &self,
        request: &str,
        history: &str,
        questions: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Selection {
            question_id: Option<String>,
            confidence: f64,
        }
        let schema = serde_json::json!({"type":"object","properties":{"question_id":{"type":["string","null"]},"confidence":{"type":"number"}},"required":["question_id","confidence"],"additionalProperties":false});
        let response = self.backend.complete_json(&self.texts.get("steps.select_question"), &serde_json::json!({"request":request,"conversation_history":history,"questions":questions}).to_string(), &schema).await?;
        let choice: Selection = contract(&response)?;
        ensure!(
            choice.confidence.is_finite()
                && (0.0..=1.0).contains(&choice.confidence)
                && choice
                    .question_id
                    .as_ref()
                    .is_none_or(|id| questions.contains_key(id)),
            "invalid question selection"
        );
        Ok(choice.question_id.filter(|_| choice.confidence >= 0.85))
    }
    async fn select_context(
        &self,
        question: &str,
        contexts: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Selection {
            context: Option<String>,
        }
        let response = self
            .backend
            .complete_json(
                &self.texts.get("steps.select_context"),
                &serde_json::json!({"request":question,"contexts":contexts}).to_string(),
                &context_schema(),
            )
            .await
            .context("context selection failed")?;
        let selected: Selection = contract(&response)?;
        ensure!(
            selected
                .context
                .as_ref()
                .is_none_or(|id| contexts.contains_key(id)),
            Unavailable(Failure::InvalidAnswer)
        );
        Ok(selected.context)
    }
    async fn select_sessions(
        &self,
        request: &str,
        history: &str,
        sessions: &std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Selection {
            sessions: Vec<String>,
        }
        let response = self.backend.complete_json(&self.texts.get("steps.select_sessions"),
            &serde_json::json!({"current_request":request,"conversation_history":history,"sessions":sessions}).to_string(),
            &sessions_schema(),
        )
        .await
        .context("session selection failed")?;
        let selected: Selection = contract(&response)?;
        ensure!(
            selected.sessions.len() <= sessions.len()
                && selected.sessions.iter().all(|k| sessions.contains_key(k)),
            Unavailable(Failure::InvalidAnswer)
        );
        Ok(selected.sessions)
    }
    async fn condense(&self, text: &str, max_chars: usize) -> Result<String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Condensed {
            text: String,
        }
        let response = self
            .backend
            .complete_json(
                &self
                    .texts
                    .render("steps.condense", &[("max_chars", &max_chars.to_string())]),
                &serde_json::json!({"message": text, "max_chars": max_chars}).to_string(),
                &condensed_schema(),
            )
            .await
            .context("message condensing failed")?;
        let condensed: Condensed = contract(&response)?;
        Ok(condensed.text)
    }
    async fn standalone_request(
        &self,
        previous_question: &str,
        previous_answer: &str,
        history: &str,
        current: &str,
    ) -> Result<Option<StandaloneRequest>> {
        let previous_answer: String = previous_answer.chars().take(1200).collect();
        let response = self.backend.complete_json(&self.texts.get("steps.standalone"),
            &serde_json::json!({"previous_request":previous_question,"previous_answer":previous_answer,"conversation_history":history,"current_request":current})
                .to_string(),
            &standalone_schema(),
        )
        .await
        .context("follow-up resolution failed")?;
        Ok(Some(contract(&response)?))
    }
    async fn classify_intent(&self, message: &str, history: &str) -> Result<IntentDecision> {
        self.classify_intent_as(message, history, Categories::default())
            .await
    }
    async fn classify_intent_as(
        &self,
        message: &str,
        history: &str,
        categories: Categories<'_>,
    ) -> Result<IntentDecision> {
        let pick = |custom: &str, default: &str| {
            if custom.trim().is_empty() {
                default.trim().to_owned()
            } else {
                custom.trim().to_owned()
            }
        };
        let response = self
            .backend
            .complete_json(
                &self.texts.render(
                    "steps.classify",
                    &[
                        (
                            "question",
                            &pick(categories.question, prompts::CLASSIFY_QUESTION),
                        ),
                        (
                            "activity_review",
                            &pick(
                                categories.activity_review,
                                prompts::CLASSIFY_ACTIVITY_REVIEW,
                            ),
                        ),
                        (
                            "personal",
                            &pick(categories.personal, prompts::CLASSIFY_PERSONAL),
                        ),
                        (
                            "statement",
                            &pick(categories.statement, prompts::CLASSIFY_STATEMENT),
                        ),
                    ],
                ),
                &serde_json::json!({"current_message":message,"conversation_history":history})
                    .to_string(),
                &intent_schema(),
            )
            .await
            .context("intent classification failed")?;
        let decision: IntentDecision = contract(&response)?;
        ensure!(
            decision.confidence.is_finite() && (0.0..=1.).contains(&decision.confidence),
            Unavailable(Failure::InvalidAnswer)
        );
        Ok(decision)
    }
    async fn select_references(
        &self,
        answer: &str,
        references: &[crate::evidence::Reference],
    ) -> Result<Vec<crate::evidence::Used>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Selection {
            used: Vec<Pick>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Pick {
            key: String,
            mention: String,
        }
        ensure!(references.len() <= 100, "too many references to select");
        if references.is_empty() {
            return Ok(Vec::new());
        }
        // Short closed keys: the model never copies long IDs or URLs.
        let listed: Vec<_> = references
            .iter()
            .enumerate()
            .map(|(i, r)| json!({"key":format!("r{}", i + 1),"kind":r.kind,"label":r.label,"project":r.project,"authority":r.authority}))
            .collect();
        let response = self
            .backend
            .complete_json(
                &self.texts.get("steps.select_references"),
                &json!({"answer":answer,"references":listed}).to_string(),
                &references_schema(),
            )
            .await
            .context("reference selection failed")?;
        let selection: Selection = contract(&response)?;
        let mut used: Vec<crate::evidence::Used> = Vec::new();
        for pick in selection.used {
            let index = pick
                .key
                .strip_prefix('r')
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| (1..=references.len()).contains(n))
                .ok_or(Unavailable(Failure::InvalidAnswer))?;
            let id = &references[index - 1].id;
            if !used.iter().any(|u| &u.id == id) {
                // Words that are not in the answer only leave the reference unlinked.
                let mention = pick.mention.trim();
                let mention =
                    if mention.chars().count() <= 200 && !mention.contains(char::is_control) {
                        mention
                    } else {
                        ""
                    };
                used.push(crate::evidence::Used {
                    id: id.clone(),
                    mention: mention.to_owned(),
                });
            }
        }
        Ok(used)
    }
    async fn plan_links(
        &self,
        reply: &str,
        offer: &crate::ado::link::LinkOffer,
    ) -> Result<Vec<crate::ado::link::PlannedLink>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Plan {
            links: Vec<crate::ado::link::PlannedLink>,
        }
        let activities: Vec<_> = offer
            .activities
            .iter()
            .map(|a| json!({"key":a.key,"date":a.date,"activity":a.label,"suggested_work_items":a.suggested}))
            .collect();
        let work_items: Vec<_> = offer
            .work_items
            .iter()
            .map(|w| json!({"id":w.id,"title":w.title}))
            .collect();
        let response = self.backend.complete_json(&self.texts.get("activity.plan_links"),
            &json!({"reply":reply,"previous_answer":offer.answer,"activities":activities,"work_items":work_items}).to_string(),
            &links_schema(),
        )
        .await
        .context("link planning failed")?;
        let plan: Plan = contract(&response)?;
        Ok(plan.links)
    }
    async fn propose_registration(
        &self,
        offer: &crate::ado::link::LinkOffer,
        language: Language,
    ) -> Result<Vec<crate::ado::link::Proposal>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Proposals {
            proposals: Vec<crate::ado::link::Proposal>,
        }
        let activities: Vec<_> = offer
            .activities
            .iter()
            .map(|a| json!({"key":a.key,"date":a.date,"activity":a.label,"suggested_work_items":a.suggested}))
            .collect();
        let work_items: Vec<_> = offer
            .work_items
            .iter()
            .map(|w| json!({"id":w.id,"title":w.title,"type":w.kind,"state":w.state,"parent":w.parent,"can_hold_new_tasks":w.holds_tasks()}))
            .collect();
        let language = match language {
            Language::Es => "Spanish",
            Language::En => "English",
        };
        let response = self
            .backend
            .complete_json(
                &self
                    .texts
                    .render("activity.propose_registration", &[("language", language)]),
                &json!({"activities":activities,"work_items":work_items}).to_string(),
                &proposals_schema(),
            )
            .await
            .context("registration proposal failed")?;
        let proposals: Proposals = contract(&response)?;
        Ok(proposals.proposals)
    }
    async fn generate(&self, input: GenerationInput<'_>) -> Result<String> {
        Ok(self.generate_response(input).await?.answer)
    }
    async fn generate_response(&self, input: GenerationInput<'_>) -> Result<GeneratedAnswer> {
        // The language rule comes first and overrides any language the free-text style names.
        let (language, teammate) = match self.language {
            Language::Es => ("Spanish (español)", "otra persona del equipo"),
            Language::En => ("English", "someone else on the team"),
        };
        // The user's own texts for each case replace the defaults; the rules around them are
        // the prompts of `answers.*`.
        let cases = input.prompts;
        let pick = |custom: &str, default: &str| {
            if custom.trim().is_empty() {
                default.trim().to_owned()
            } else {
                custom.trim().to_owned()
            }
        };
        let texts = &self.texts;
        let (status_round, recap) = if cases.questions {
            (
                texts.get("answers.questions_status_round"),
                texts.get("answers.questions_recap"),
            )
        } else {
            (prompts::STATUS_ROUND.to_owned(), prompts::RECAP.to_owned())
        };
        let mut system = texts.render(
            "answers.frame",
            &[
                ("language", language),
                ("teammate", teammate),
                ("general", &pick(cases.general, prompts::GENERAL)),
                ("status_round", &pick(cases.status_round, &status_round)),
                ("recap", &pick(cases.recap, &recap)),
                (
                    "documentation",
                    &pick(cases.documentation, prompts::DOCUMENTATION),
                ),
                ("style", &self.style),
            ],
        );
        // Each rule the request needs, from the user's prompts.
        let add = |system: &mut String, id: &str| {
            system.push(' ');
            system.push_str(texts.get(id).trim());
        };
        if input.review {
            add(&mut system, "answers.review");
            system.push(' ');
            system.push_str(&pick(cases.activity_review, prompts::ACTIVITY_REVIEW));
            add(
                &mut system,
                if input.proposing {
                    "answers.review_proposing"
                } else {
                    "answers.review_reading"
                },
            );
        }
        if input.unprompted {
            system.push(' ');
            system.push_str(&pick(cases.statement, prompts::STATEMENT));
        }
        if input.in_progress {
            add(&mut system, "answers.in_progress");
        }
        if !input.recipient.is_empty() {
            add(&mut system, "answers.recipient");
        }
        if !input.evaluations.is_empty() {
            add(&mut system, "answers.evaluations");
        }
        if !input.examples.is_empty() {
            add(&mut system, "answers.examples");
        }
        if !input.context_instructions.is_empty() {
            add(&mut system, "answers.context");
        }
        if !input.guidance.is_empty() || !input.previous_draft.is_empty() {
            if !input.previous_draft.is_empty() {
                add(&mut system, "answers.previous_draft");
            }
            add(&mut system, "answers.guidance");
            if !input.previous_draft.is_empty() {
                add(&mut system, "answers.previous_draft_keep");
            }
        }
        add(&mut system, "answers.contract");
        // No tools and no token or character cap: only bounded authorized evidence reaches
        // the provider. Teams' message size is still checked before sending.
        let response = self
            .backend
            .complete_json(&system, &serde_json::to_string(&input)?, &answer_schema())
            .await
            .context("LLM generation failed")?;
        let mut answer: GeneratedAnswer = contract(&response)?;
        answer.provider = Some(self.backend.label().into());
        Ok(answer)
    }
}

/// Providers in priority order. Every call starts from the first: a provider that ran out of
/// credits on one call answers again as soon as its usage returns. Only model calls fall back;
/// nothing here touches Graph, so a fallback never duplicates a Teams message.
pub struct Chain {
    members: Vec<Model>,
    name: String,
}
impl Chain {
    pub fn new(members: Vec<Model>) -> Result<Self> {
        ensure!(!members.is_empty(), "enable at least one LLM provider");
        let name = members
            .iter()
            .map(|m| m.name())
            .collect::<Vec<_>>()
            .join(" > ");
        Ok(Self { members, name })
    }
    /// The user's prompts, for every member.
    pub fn texts(mut self, texts: std::sync::Arc<crate::agent_prompts::Texts>) -> Self {
        for member in &mut self.members {
            member.texts = texts.clone();
        }
        self
    }
    async fn first<'a, T>(
        &'a self,
        operation: &'static str,
        call: impl Fn(&'a Model) -> std::pin::Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>
        + Send,
    ) -> Result<(T, Vec<String>)> {
        let mut failure = None;
        let mut skipped = Vec::new();
        for (position, member) in self.members.iter().enumerate() {
            match call(member).await {
                Ok(value) => {
                    if position > 0 {
                        tracing::info!(
                            event = "llm_fallback_used",
                            operation,
                            provider = member.name()
                        );
                    }
                    return Ok((value, skipped));
                }
                Err(error) => {
                    let kind = failure_of(&error);
                    tracing::warn!(
                        event = "llm_provider_unavailable",
                        operation,
                        provider = member.name(),
                        failure = kind.as_str()
                    );
                    skipped.push(format!("{}: {}", member.name(), kind.as_str()));
                    failure = Some(error);
                }
            }
        }
        Err(failure.unwrap_or_else(|| anyhow::anyhow!("no LLM provider")))
    }
}
#[async_trait]
impl LlmProvider for Chain {
    async fn plan_activity_tasks(
        &self,
        activities: &[crate::ado::link::Linkable],
        parents: &[crate::ado::link::Target],
        evidence: &str,
        remaining: f64,
        context: &str,
    ) -> Result<Vec<crate::activity::Draft>> {
        self.first("plan_activity_tasks", |p| {
            Box::pin(p.plan_activity_tasks(activities, parents, evidence, remaining, context))
        })
        .await
        .map(|(value, _)| value)
    }
    fn name(&self) -> &str {
        &self.name
    }
    async fn standalone_request(
        &self,
        previous_question: &str,
        previous_answer: &str,
        history: &str,
        current: &str,
    ) -> Result<Option<StandaloneRequest>> {
        Ok(self
            .first("standalone_request", |m| {
                m.standalone_request(previous_question, previous_answer, history, current)
            })
            .await?
            .0)
    }
    async fn select_tool(
        &self,
        question: &str,
        candidates: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        Ok(self
            .first("select_tool", |m| m.select_tool(question, candidates))
            .await?
            .0)
    }
    async fn classify_intent(&self, message: &str, history: &str) -> Result<IntentDecision> {
        self.classify_intent_as(message, history, Categories::default())
            .await
    }
    async fn classify_intent_as(
        &self,
        message: &str,
        history: &str,
        categories: Categories<'_>,
    ) -> Result<IntentDecision> {
        Ok(self
            .first("classify_intent", |m| {
                m.classify_intent_as(message, history, categories)
            })
            .await?
            .0)
    }
    async fn identify_question(
        &self,
        request: &str,
        history: &str,
        questions: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        Ok(self
            .first("identify_question", |m| {
                m.identify_question(request, history, questions)
            })
            .await?
            .0)
    }
    async fn select_context(
        &self,
        question: &str,
        contexts: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        Ok(self
            .first("select_context", |m| m.select_context(question, contexts))
            .await?
            .0)
    }
    async fn select_sessions(
        &self,
        request: &str,
        history: &str,
        sessions: &std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        Ok(self
            .first("select_sessions", |m| {
                m.select_sessions(request, history, sessions)
            })
            .await?
            .0)
    }
    async fn select_references(
        &self,
        answer: &str,
        references: &[crate::evidence::Reference],
    ) -> Result<Vec<crate::evidence::Used>> {
        Ok(self
            .first("select_references", |m| {
                m.select_references(answer, references)
            })
            .await?
            .0)
    }
    async fn propose_registration(
        &self,
        offer: &crate::ado::link::LinkOffer,
        language: Language,
    ) -> Result<Vec<crate::ado::link::Proposal>> {
        Ok(self
            .first("propose_registration", |m| {
                m.propose_registration(offer, language)
            })
            .await?
            .0)
    }
    async fn condense(&self, text: &str, max_chars: usize) -> Result<String> {
        Ok(self
            .first("condense", |m| m.condense(text, max_chars))
            .await?
            .0)
    }
    async fn plan_links(
        &self,
        reply: &str,
        offer: &crate::ado::link::LinkOffer,
    ) -> Result<Vec<crate::ado::link::PlannedLink>> {
        Ok(self
            .first("plan_links", |m| m.plan_links(reply, offer))
            .await?
            .0)
    }
    async fn generate(&self, input: GenerationInput<'_>) -> Result<String> {
        Ok(self.generate_response(input).await?.answer)
    }
    async fn generate_response(&self, input: GenerationInput<'_>) -> Result<GeneratedAnswer> {
        let input = &input;
        self.first("generate_response", |m| m.generate_response(*input))
            .await
            .map(|(mut answer, fallbacks)| {
                answer.fallbacks = fallbacks;
                answer
            })
    }
}

const DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com";

/// The configured provider chain. `DEEPSEEK_API_KEY` is read only when DeepSeek is enabled;
/// CLIs authenticate with their own login. Returns the chain and the secrets it loaded, which
/// the caller adds to the redactor.
pub fn from_config(llm: &Llm) -> Result<(Chain, Vec<String>)> {
    validate(llm)?;
    let mut secrets = Vec::new();
    let mut members = Vec::new();
    for choice in llm.active() {
        members.push(model_for(&choice, llm, &mut secrets)?);
    }
    Ok((Chain::new(members)?, secrets))
}

/// One provider on its own, as `kuroko test providers` probes each member of the chain.
pub fn model_for(choice: &LlmChoice, llm: &Llm, secrets: &mut Vec<String>) -> Result<Model> {
    let style = llm.style.as_str();
    let model = match choice.provider.as_str() {
        "deepseek" => {
            let key = crate::security::secret("DEEPSEEK_API_KEY")?;
            secrets.push(key.clone());
            Model::new(
                DeepSeek::new(&key, &choice.model, &choice.effort, DEEPSEEK_ENDPOINT)?,
                style,
            )
        }
        "codex" => Model::new(CodexCli::new(&choice.model, &choice.effort), style),
        "claude" => Model::new(ClaudeCli::new(&choice.model, &choice.effort), style),
        "opencode" => Model::new(OpenCodeCli::new(&choice.model, &choice.effort), style),
        _ => bail!("unsupported LLM provider"),
    };
    Ok(model.language(llm.language))
}

/// Whether the configured chain calls the DeepSeek API (and so needs its key).
pub fn uses_deepseek(llm: &Llm) -> bool {
    llm.active().iter().any(|c| c.provider == "deepseek")
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    fn llm(chain: Vec<LlmChoice>) -> Llm {
        Llm {
            provider: "deepseek".into(),
            model: "deepseek-flash".into(),
            style: "Brief".into(),
            language: Language::Es,
            chain,
        }
    }
    fn choice(provider: &str, model: &str, effort: &str) -> LlmChoice {
        LlmChoice {
            provider: provider.into(),
            model: model.into(),
            effort: effort.into(),
            enabled: true,
        }
    }

    #[test]
    fn provider_selection_fails_closed() {
        for name in PROVIDERS {
            assert!(validate_provider(name).is_ok());
        }
        for name in ["unknown", "openai", "http://evil", "Codex"] {
            assert!(validate_provider(name).is_err());
        }
    }

    #[test]
    fn empty_chain_keeps_the_legacy_deepseek_profile() {
        let legacy = llm(Vec::new());
        assert!(validate(&legacy).is_ok());
        assert_eq!(
            legacy.active(),
            vec![choice("deepseek", "deepseek-flash", "max")]
        );
        assert!(uses_deepseek(&legacy));
    }

    #[test]
    fn chain_validation_rejects_unsafe_or_ambiguous_entries() {
        let valid = llm(vec![
            choice("codex", "gpt-6.1-sol", "medium"),
            choice("claude", "claude-opus-5-5", "medium"),
            choice("deepseek", "deepseek-flash", "max"),
            choice("opencode", "example/a", "none"),
            choice("opencode", "example/b", "minimal"),
            choice("opencode", "example/b", "deep"),
        ]);
        assert!(validate(&valid).is_ok());
        let mut disabled = valid.clone();
        disabled.chain[2].enabled = false;
        assert!(validate(&disabled).is_ok());
        assert!(!uses_deepseek(&disabled));
        assert_eq!(disabled.active().len(), 5);
        for chain in [
            vec![choice("openai", "gpt", "high")],
            vec![choice("codex", "--dangerously-bypass", "medium")],
            vec![choice("codex", "gpt 6", "medium")],
            vec![choice("codex", "", "medium")],
            vec![choice("codex", "gpt-6.1-sol", "ultra")],
            vec![choice("claude", "claude-opus-5-5", "none")],
            vec![choice("deepseek", "deepseek-flash", "medium")],
            vec![choice("opencode", "model-without-provider", "default")],
            vec![choice("opencode", "provider/", "default")],
            vec![choice("opencode", "provider/model", "bad#variant")],
            vec![choice("opencode", "provider/model", "--variant")],
            vec![choice("opencode", "provider/model", "")],
            vec![choice("opencode", "provider/model", "two words")],
            vec![choice("codex", "gpt-6.1-sol", "low"); MAX_CHAIN_LENGTH + 1],
        ] {
            assert!(validate(&llm(chain)).is_err());
        }
        let mut none_enabled = valid;
        for entry in &mut none_enabled.chain {
            entry.enabled = false;
        }
        assert!(validate(&none_enabled).is_err());
    }

    #[test]
    fn chain_keeps_repeated_providers_models_and_efforts_in_order() {
        let config = llm(vec![
            choice("codex", "gpt-6.1-sol", "medium"),
            choice("codex", "gpt-6-luna", "low"),
            choice("codex", "gpt-6.1-sol", "high"),
            choice("opencode", "deepseek/deepseek-chat", "default"),
        ]);
        validate(&config).unwrap();
        assert_eq!(config.active(), config.chain);
    }

    #[test]
    fn usage_limits_are_classified_without_exposing_text() {
        for text in [
            "Fable 5.1 requires usage credits. Switch to another model",
            "You've hit your usage limit. Upgrade or try again later.",
            "{\"type\":\"error\",\"status\":429,\"error\":{}}",
            "Claude AI usage limit reached|1760000000",
            "Insufficient Balance",
        ] {
            assert!(usage_limited(text), "{text}");
        }
        assert!(!usage_limited("The 'x' model is not supported"));
        let error =
            anyhow::Error::new(Unavailable(Failure::UsageLimit)).context("tool selection failed");
        assert_eq!(failure_of(&error), Failure::UsageLimit);
        assert!(!format!("{error:#}").contains("Fable"));
    }

    /// Answers unless `exhausted`; counts calls.
    struct Fake {
        label: &'static str,
        exhausted: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Backend for Fake {
        fn label(&self) -> &str {
            self.label
        }
        async fn complete_json(&self, _: &str, _: &str, schema: &Value) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            ensure!(
                !self.exhausted.load(Ordering::SeqCst),
                Unavailable(Failure::UsageLimit)
            );
            Ok(match schema["required"][0].as_str() {
                Some("source") => json!({"source":"wiki"}),
                Some("intent") => json!({"intent":"activity_review","confidence":0.9}),
                Some("used") => json!({"used":[{"key":"r2","mention":"la página B"}]}),
                _ => json!({"answer":self.label,"detailed":false}),
            }
            .to_string())
        }
    }
    fn fake(label: &'static str) -> (Model, Arc<AtomicBool>, Arc<AtomicUsize>) {
        let exhausted = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let model = Model::new(
            Fake {
                label,
                exhausted: exhausted.clone(),
                calls: calls.clone(),
            },
            "Brief",
        );
        (model, exhausted, calls)
    }
    fn input() -> GenerationInput<'static> {
        GenerationInput {
            question: "¿Horario?",
            evidence: "Soporte de 9 a 18.",
            detail_requested: false,
            history: "",
            review: false,
            proposing: false,
            unprompted: false,
            in_progress: false,
            guidance: "",
            previous_draft: "",
            context_instructions: "",
            prompts: Default::default(),
            recipient: "",
            examples: "",
            evaluations: "",
        }
    }

    #[tokio::test]
    async fn fallback_is_evaluated_again_on_every_call() {
        let (codex, codex_out, codex_calls) = fake("codex:gpt-6.1-sol:medium");
        let (claude, claude_out, _) = fake("claude:claude-opus-5-5:medium");
        let (deepseek, _, deepseek_calls) = fake("deepseek:deepseek-flash:max");
        let chain = Chain::new(vec![codex, claude, deepseek]).unwrap();
        assert_eq!(
            chain.name(),
            "codex:gpt-6.1-sol:medium > claude:claude-opus-5-5:medium > deepseek:deepseek-flash:max"
        );

        let answer = chain.generate_response(input()).await.unwrap();
        assert_eq!(answer.provider.as_deref(), Some("codex:gpt-6.1-sol:medium"));

        // Codex runs out of credits, then Claude too: the third provider answers.
        codex_out.store(true, Ordering::SeqCst);
        let answer = chain.generate_response(input()).await.unwrap();
        assert_eq!(
            answer.provider.as_deref(),
            Some("claude:claude-opus-5-5:medium")
        );
        claude_out.store(true, Ordering::SeqCst);
        let answer = chain.generate_response(input()).await.unwrap();
        assert_eq!(
            answer.provider.as_deref(),
            Some("deepseek:deepseek-flash:max")
        );
        assert_eq!(deepseek_calls.load(Ordering::SeqCst), 1);

        // Usage returns: the next call goes back to the default provider.
        codex_out.store(false, Ordering::SeqCst);
        let calls_before = codex_calls.load(Ordering::SeqCst);
        let answer = chain.generate_response(input()).await.unwrap();
        assert_eq!(answer.provider.as_deref(), Some("codex:gpt-6.1-sol:medium"));
        assert_eq!(codex_calls.load(Ordering::SeqCst), calls_before + 1);
        assert_eq!(deepseek_calls.load(Ordering::SeqCst), 1);

        // Tool selection falls back the same way and keeps the closed ID check.
        codex_out.store(true, Ordering::SeqCst);
        claude_out.store(false, Ordering::SeqCst);
        let candidates =
            std::collections::BTreeMap::from([("wiki".to_string(), "Docs".to_string())]);
        assert_eq!(
            chain
                .select_tool("¿Cómo?", &candidates)
                .await
                .unwrap()
                .as_deref(),
            Some("wiki")
        );
    }

    #[tokio::test]
    async fn exhausted_chain_fails_with_the_last_failure_class() {
        let (codex, codex_out, _) = fake("codex:gpt-6.1-sol:medium");
        let (claude, claude_out, _) = fake("claude:claude-opus-5-5:medium");
        codex_out.store(true, Ordering::SeqCst);
        claude_out.store(true, Ordering::SeqCst);
        let chain = Chain::new(vec![codex, claude]).unwrap();
        let error = chain.generate_response(input()).await.unwrap_err();
        assert_eq!(failure_of(&error), Failure::UsageLimit);
        assert!(Chain::new(Vec::new()).is_err());
    }

    /// Keeps the system prompt of the last call.
    struct Capture(Arc<std::sync::Mutex<String>>);
    #[async_trait]
    impl Backend for Capture {
        fn label(&self) -> &str {
            "capture"
        }
        async fn complete_json(&self, system: &str, _: &str, _: &Value) -> Result<String> {
            *self.0.lock().unwrap() = system.to_owned();
            Ok(json!({"answer":"ok","detailed":false}).to_string())
        }
    }

    #[tokio::test]
    async fn the_users_case_texts_replace_the_defaults_but_not_the_rules() {
        let system = Arc::new(std::sync::Mutex::new(String::new()));
        let model = Model::new(Capture(system.clone()), "");
        model.generate_response(input()).await.unwrap();
        let prompt = system.lock().unwrap().clone();
        for default in [
            prompts::GENERAL,
            prompts::STATUS_ROUND,
            prompts::RECAP,
            prompts::DOCUMENTATION,
        ] {
            assert!(prompt.contains(default.trim()), "{default}");
        }
        model
            .generate_response(GenerationInput {
                prompts: Prompts {
                    status_round: "Responde la ronda en tres viñetas.",
                    general: "Tutea siempre y sé breve.",
                    ..Default::default()
                },
                ..input()
            })
            .await
            .unwrap();
        let prompt = system.lock().unwrap().clone();
        assert!(prompt.contains("Responde la ronda en tres viñetas."));
        assert!(prompt.contains("Tutea siempre y sé breve."));
        assert!(!prompt.contains(prompts::STATUS_ROUND.trim()));
        assert!(!prompt.contains(prompts::GENERAL.trim()));
        // The rules around them stay.
        assert!(prompt.contains(prompts::RECAP.trim()));
        assert!(prompt.contains("UNTRUSTED DATA"));
        assert!(prompt.contains("Do not invent URLs."));
        // A review's base text is replaced; it still cannot offer retired task writes.
        model
            .generate_response(GenerationInput {
                review: true,
                prompts: Prompts {
                    activity_review: "Lista solo lo no registrado.",
                    ..Default::default()
                },
                ..input()
            })
            .await
            .unwrap();
        let prompt = system.lock().unwrap().clone();
        assert!(prompt.contains("This request is an activity review"));
        assert!(prompt.contains("Lista solo lo no registrado."));
        assert!(prompt.contains("Do not propose creating tasks"));
        assert!(!prompt.contains(prompts::ACTIVITY_REVIEW.trim()));
    }

    #[tokio::test]
    async fn the_users_category_descriptions_reach_the_classifier() {
        struct Prompted(Arc<std::sync::Mutex<String>>);
        #[async_trait]
        impl Backend for Prompted {
            fn label(&self) -> &str {
                "prompted"
            }
            async fn complete_json(&self, system: &str, _: &str, _: &Value) -> Result<String> {
                *self.0.lock().unwrap() = system.to_owned();
                Ok(json!({"intent":"question","confidence":0.9}).to_string())
            }
        }
        let system = Arc::new(std::sync::Mutex::new(String::new()));
        let model = Model::new(Prompted(system.clone()), "");
        model.classify_intent("hola", "").await.unwrap();
        let prompt = system.lock().unwrap().clone();
        assert!(prompt.contains(prompts::CLASSIFY_ACTIVITY_REVIEW.trim()));
        assert!(
            prompt.contains("{\"intent\":\"question|activity_review|personal|greeting|statement\"")
        );
        model
            .classify_intent_as(
                "hola",
                "",
                Categories {
                    activity_review: "pide revisar mi trabajo sin registrar o mis horas faltantes",
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let prompt = system.lock().unwrap().clone();
        assert!(prompt.contains(
            "activity_review (pide revisar mi trabajo sin registrar o mis horas faltantes)"
        ));
        assert!(prompt.contains(prompts::CLASSIFY_PERSONAL.trim()));
    }

    #[tokio::test]
    async fn the_answer_language_overrides_the_style() {
        let system = Arc::new(std::sync::Mutex::new(String::new()));
        let spanish = Model::new(Capture(system.clone()), "Natural English");
        spanish.generate_response(input()).await.unwrap();
        let prompt = system.lock().unwrap().clone();
        assert!(prompt.starts_with("Write the answer in natural Spanish (español), even if"));
        assert!(prompt.contains("'otra persona del equipo'"));
        assert!(!prompt.contains("activity review"));
        // Natural voice: no internal jargon, stock disclaimers or menus of options.
        assert!(prompt.contains("never use stock disclaimers"));
        assert!(prompt.contains("never vulgar, slangy or overly casual"));
        assert!(prompt.contains("a menu of options"));
        // A status round: progress from the item's state and owner, dated commitments and
        // the pending items they depend on.
        assert!(prompt.contains("A status round about the user's work"));
        assert!(prompt.contains("who holds it now, its current assignee"));
        assert!(prompt.contains("plan a production deployment (paso a producción)"));
        assert!(prompt.contains("what the user did on a given day or period"));
        assert!(!prompt.contains("owner_guidance"));
        spanish
            .generate_response(GenerationInput {
                guidance: "Ya está en validación.",
                previous_draft: "No tengo a mano el avance.",
                context_instructions: "",
                prompts: Default::default(),
                recipient: "",
                examples: "",
                evaluations: "",
                ..input()
            })
            .await
            .unwrap();
        let prompt = system.lock().unwrap().clone();
        assert!(prompt.contains("owner_guidance, when present, is the user's own context"));
        assert!(prompt.contains("never invent others or any URL"));
        assert!(!prompt.contains("The current request asks no question"));
        // A message left unanswered is replied to, not answered as a question.
        spanish
            .generate_response(GenerationInput {
                unprompted: true,
                in_progress: false,
                ..input()
            })
            .await
            .unwrap();
        let prompt = system.lock().unwrap().clone();
        assert!(prompt.contains("The current request asks no question"));
        assert!(!prompt.contains("previous_draft"));

        let english = Model::new(Capture(system.clone()), "Español natural").language(Language::En);
        english
            .generate_response(GenerationInput {
                review: true,
                ..input()
            })
            .await
            .unwrap();
        let prompt = system.lock().unwrap().clone();
        assert!(prompt.starts_with("Write the answer in natural English, even if the request, the evidence or the style are in another language"));
        assert!(prompt.contains("'someone else on the team'"));
        assert!(!prompt.contains("otra persona del equipo"));
        // An activity review lets verified authorship speak in the first person.
        assert!(prompt.contains("This request is an activity review"));
        assert!(prompt.contains("without asking for confirmation"));
    }

    #[tokio::test]
    async fn activity_drafts_follow_the_configured_language() {
        let system = Arc::new(std::sync::Mutex::new(String::new()));
        for (language, name) in [(Language::Es, "Spanish"), (Language::En, "English")] {
            let model = Model::new(Capture(system.clone()), "").language(language);
            // Capture's reply breaks the planning contract; only the prompt matters here.
            let _ = model.plan_activity_tasks(&[], &[], "", 9., "").await;
            let prompt = system.lock().unwrap().clone();
            assert!(prompt.contains(&format!(
                "Write titles, descriptions and clarification questions in {name}"
            )));
            // Completion evidence is the agent's own words, never chat content.
            assert!(prompt.contains(&format!("evidence: in {name}")));
            assert!(prompt.contains("Never quote, paraphrase or reveal chat messages"));
        }
    }

    /// Answers every call with a fixed JSON text.
    struct Reply(&'static str);
    #[async_trait]
    impl Backend for Reply {
        fn label(&self) -> &str {
            "reply"
        }
        async fn complete_json(&self, _: &str, _: &str, _: &Value) -> Result<String> {
            Ok(self.0.into())
        }
    }
    fn reference(id: &str) -> crate::evidence::Reference {
        crate::evidence::Reference {
            id: id.into(),
            kind: "wiki".into(),
            label: id.into(),
            url: "https://dev.azure.com/example/project/_wiki".into(),
            organization: "https://dev.azure.com/example".into(),
            project: "project".into(),
            aliases: vec![],
            parent: None,
            revision: None,
            authority: None,
            author: None,
            author_role: None,
        }
    }

    #[tokio::test]
    async fn intent_and_reference_contracts_are_closed() {
        let intent = Model::new(
            Reply(r#"{"intent":"activity_review","confidence":0.8}"#),
            "",
        )
        .classify_intent("¿qué hice sin tarea?", "")
        .await
        .unwrap();
        assert_eq!(intent.intent, Intent::ActivityReview);
        assert!(intent.confident());
        for invalid in [
            r#"{"intent":"approve","confidence":0.9}"#,
            r#"{"intent":"question","confidence":1.5}"#,
            r#"{"intent":"question"}"#,
            r#"{"intent":"question","confidence":0.9,"tools":["x"]}"#,
        ] {
            let error = Model::new(Reply(invalid), "")
                .classify_intent("hola", "")
                .await
                .unwrap_err();
            assert_eq!(failure_of(&error), Failure::InvalidAnswer, "{invalid}");
        }

        let references = [reference("wiki:a"), reference("wiki:b")];
        let used = Model::new(
            Reply(r#"{"used":[{"key":"r2","mention":" la página B "},{"key":"r2","mention":""}]}"#),
            "",
        )
        .select_references("Respuesta", &references)
        .await
        .unwrap();
        assert_eq!(
            used,
            vec![crate::evidence::Used {
                id: "wiki:b".into(),
                mention: "la página B".into()
            }]
        );
        // Words with control characters are dropped; the reference stays selected.
        let used = Model::new(Reply(r#"{"used":[{"key":"r1","mention":"a\nb"}]}"#), "")
            .select_references("Respuesta", &references)
            .await
            .unwrap();
        assert_eq!(used[0].mention, "");
        for invalid in [
            r#"{"used":[{"key":"r3","mention":""}]}"#,
            r#"{"used":[{"key":"wiki:a","mention":""}]}"#,
            r#"{"used":[{"key":"r0","mention":""}]}"#,
            r#"{"used":["r1"]}"#,
            r#"{"used":[{"key":"r1"}]}"#,
            r#"{"used":[{"key":"r1","mention":"","url":"https://x"}]}"#,
        ] {
            let error = Model::new(Reply(invalid), "")
                .select_references("Respuesta", &references)
                .await
                .unwrap_err();
            assert_eq!(failure_of(&error), Failure::InvalidAnswer, "{invalid}");
        }
        assert!(
            Model::new(Reply("{}"), "")
                .select_references("Respuesta", &[])
                .await
                .unwrap()
                .is_empty()
        );

        // A registration proposal is closed too: an action outside link/create or extra
        // fields are invalid.
        let proposal = Model::new(
            Reply(
                r#"{"proposals":[{"activity":"a2","action":"create","work_item":null,"parent":50,"title":"Fix","reason":"Same HU."}]}"#,
            ),
            "",
        )
        .propose_registration(&crate::ado::link::LinkOffer::default(), Language::Es)
        .await
        .unwrap();
        assert_eq!(proposal[0].action, crate::ado::link::ProposalAction::Create);
        assert_eq!(proposal[0].parent, Some(50));
        for invalid in [
            r#"{"proposals":[{"activity":"a1","action":"delete","work_item":1,"parent":null,"title":null,"reason":""}]}"#,
            r#"{"proposals":[{"activity":"a1","action":"link","work_item":1,"parent":null,"title":null,"reason":"","url":"x"}]}"#,
            r#"{"proposals":[{"activity":"a1","action":"link","work_item":1}]}"#,
        ] {
            let error = Model::new(Reply(invalid), "")
                .propose_registration(&crate::ado::link::LinkOffer::default(), Language::Es)
                .await
                .unwrap_err();
            assert_eq!(failure_of(&error), Failure::InvalidAnswer, "{invalid}");
        }

        // A link plan names only keys and IDs; anything else is an invalid answer.
        let offer = crate::ado::link::LinkOffer::default();
        let plan = Model::new(Reply(r#"{"links":[{"activity":"a1","work_item":25}]}"#), "")
            .plan_links("sí, en la #25", &offer)
            .await
            .unwrap();
        assert_eq!(
            plan,
            vec![crate::ado::link::PlannedLink {
                activity: "a1".into(),
                work_item: 25
            }]
        );
        for invalid in [
            r#"{"links":[{"activity":"a1","work_item":25,"url":"https://x"}]}"#,
            r#"{"links":[{"activity":"a1","work_item":"25"}]}"#,
            r#"{"links":[{"activity":"a1","work_item":-1}]}"#,
            r#"{"links":[],"comment":"x"}"#,
        ] {
            let error = Model::new(Reply(invalid), "")
                .plan_links("sí", &offer)
                .await
                .unwrap_err();
            assert_eq!(failure_of(&error), Failure::InvalidAnswer, "{invalid}");
        }
    }

    #[tokio::test]
    async fn intent_and_reference_selection_fall_back_like_answers() {
        let (codex, codex_out, _) = fake("codex:gpt-6.1-sol:medium");
        let (claude, _, claude_calls) = fake("claude:claude-opus-5-5:medium");
        codex_out.store(true, Ordering::SeqCst);
        let chain = Chain::new(vec![codex, claude]).unwrap();
        let intent = chain
            .classify_intent("revisa mi trabajo", "")
            .await
            .unwrap();
        assert_eq!(intent.intent, Intent::ActivityReview);
        let used = chain
            .select_references("Respuesta", &[reference("wiki:a"), reference("wiki:b")])
            .await
            .unwrap();
        assert_eq!(used[0].id, "wiki:b");
        assert_eq!(used[0].mention, "la página B");
        assert_eq!(claude_calls.load(Ordering::SeqCst), 2);
    }
}
