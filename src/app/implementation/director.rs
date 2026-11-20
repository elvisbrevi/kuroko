//! The workflow director's session. It runs on the director's machine in the same tool-less,
//! read-only, closed-JSON session as planning (`llm::implementation::direct`), with Remote
//! Control like every agent session: a Claude session that answered stays reachable for the
//! person, yielding its place to newer sessions when the machine keeps the most it can. It runs
//! in a private directory of its own, `implementation/directors/<record ID>` (kept while the session
//! may be reachable, removed by a later session after `DIRECTORY_HOURS`), with the restricted
//! child environment;
//! on another machine it is the additive one-shot call `workflow_direct` (`answer`), which a
//! worker runs beside its poll and advertises with `workflow_direct_support`. The coordinator
//! records each session as an agent session in `request_runs` (purpose `classification`: the
//! machine, provider, model, effort, native session ID, timestamps and the classified item's
//! verified link), so Agent history lists it; startup marks one still generating
//! `interrupted`. Nothing here starts a run or writes to a provider.
use super::*;
use crate::llm::implementation::{Cause, Direction, Failed, Failure};

/// How long listing the machine's installed CLIs may take before a session, when the last
/// probe (`llm::implementation::PROBE_REUSE`) does not offer the agent.
pub(in crate::app) const PROBE_SECONDS: u64 = 15;
/// How long one director session may take on its machine.
pub(in crate::app) const SESSION_SECONDS: u64 = 60;
/// A worker polls every two seconds: one poll to claim the call, one to return its answer,
/// with room for a slow round-trip.
pub(in crate::app) const POLL_SECONDS: u64 = 10;
/// How long the coordinator waits for another machine's answer: the probe, the session and the
/// polls, with ten seconds of margin (95). With the item read before it, a classification
/// answers within the control channel's client deadline (`control::CLIENT_DEADLINE_SECONDS`).
pub(in crate::app) const WAIT_SECONDS: u64 = PROBE_SECONDS + SESSION_SECONDS + POLL_SECONDS + 10;
/// How long a director's directory is kept: longer than a retained session (two hours).
const DIRECTORY_HOURS: u64 = 3;
const INSTRUCTIONS: usize = 12_000;
const INPUT: usize = 200_000;

/// What the coordinator asks the director.
pub(in crate::app) struct Ask<'a> {
    pub machine: &'a str,
    /// Choosing a workflow for an item, or summarizing a run (the record's `purpose`).
    pub task: crate::llm::implementation::DirectTask,
    pub director: &'a crate::workflows::director::Director,
    /// The resolved `direct` prompt.
    pub instructions: &'a str,
    /// The item and the candidate workflows, as JSON (untrusted input to the director).
    pub input: String,
    /// The classified item, read fresh: its verified link goes in the record.
    pub item: &'a crate::tools::todo::Item,
    /// The session's own bound, when it is not the director's (`SESSION_SECONDS`): the
    /// supervisor's `session_seconds`.
    pub seconds: Option<u64>,
}
/// The recorded session and what it returned.
pub(in crate::app) struct Answer {
    pub run: String,
    pub session_id: Option<String>,
    pub direction: Direction,
}

/// The call a director's machine answers (`workflow_direct`).
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Call {
    /// Left out for choosing a workflow, so an older worker, which does not know the field,
    /// still answers it.
    #[serde(
        default,
        skip_serializing_if = "crate::llm::implementation::DirectTask::is_classify"
    )]
    task: crate::llm::implementation::DirectTask,
    id: String,
    provider: String,
    model: String,
    effort: String,
    instructions: String,
    input: String,
    /// The item's ID, for the session's name; sent only to a worker that advertises
    /// `session_names_support` (an older one refuses unknown fields) and absent from an older
    /// coordinator's call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item: Option<u64>,
    /// The session's bound when it is not `SESSION_SECONDS` (a supervisor's look); left out
    /// otherwise, and sent only to a worker that advertises `workflow_supervise_support`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seconds: Option<u64>,
    /// The user's changes to the prompts the session composes (`DirectTask::changes`); sent only
    /// to a worker that advertises `prompt_texts_support`, which uses its own default for the
    /// rest.
    #[serde(default, skip_serializing_if = "crate::agent_prompts::Texts::is_empty")]
    texts: crate::agent_prompts::Texts,
}

/// The longest session a call may ask for (the supervisor's `session_seconds` bound).
const MAX_SESSION_SECONDS: u64 = 600;
/// How long the coordinator waits for another machine's answer to a session bounded to
/// `seconds`: the probe, the session and the polls, with ten seconds of margin.
pub(in crate::app) fn wait_seconds(seconds: u64) -> u64 {
    PROBE_SECONDS + seconds + POLL_SECONDS + 10
}

fn valid_session(session: &str) -> Result<()> {
    ensure!(
        (1..=200).contains(&session.len())
            && session
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
        "Invalid session ID."
    );
    Ok(())
}
fn now_text() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Run the director's session on this machine.
async fn session(
    host: &Arc<Host>,
    call: &Call,
    observer: crate::llm::implementation::SessionObserver<'_>,
    remote: crate::llm::implementation::mobile::Observer,
) -> Result<Direction> {
    uuid(&call.id)?;
    ensure!(
        call.instructions.chars().count() <= INSTRUCTIONS && call.input.len() <= INPUT,
        "The workflow director's input is too long."
    );
    let seconds = call.seconds.unwrap_or(SESSION_SECONDS);
    ensure!(
        (1..=MAX_SESSION_SECONDS).contains(&seconds),
        "Invalid session bound."
    );
    crate::llm::implementation::validate_recent(
        &call.provider,
        &call.model,
        &call.effort,
        Duration::from_secs(PROBE_SECONDS),
    )
    .await?;
    // A private directory for this session only. A retained session keeps working in it, so
    // it outlives the session; a later one removes it once no session can be reachable.
    let parent = host.state.implementation.root.join("directors");
    security::private_dir(&parent)?;
    sweep(&parent);
    let directory = parent.join(&call.id);
    security::private_dir(&directory)?;
    match tokio::time::timeout(
        Duration::from_secs(seconds),
        crate::llm::implementation::direct(
            call.task,
            (&call.provider, &call.model, &call.effort),
            directory,
            &call.instructions,
            &call.input,
            // Changes another host sent that this one cannot accept leave its own defaults.
            &Some(call.texts.clone())
                .filter(|t| t.check().is_ok())
                .unwrap_or_default(),
            &call.task.session_name(call.item),
            observer,
            remote,
        ),
    )
    .await
    {
        Ok(direction) => direction,
        // The session's process tree ends with it; it was read-only and wrote nothing.
        Err(_) => Ok(Direction {
            answer: None,
            problem: Some("The workflow director did not answer in time."),
            failure: Some(Failure::of(Cause::Timeout)),
        }),
    }
}

/// Remove the directories of director sessions that no Remote Control can still reach.
fn sweep(parent: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let old = Duration::from_secs(DIRECTORY_HOURS * 3600);
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > old);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// `workflow_direct` on the director's machine: run the session and return what it gave, with
/// its native session ID, times and Remote Control access. The coordinator keeps the record.
pub(super) async fn answer(host: &Arc<Host>, args: &Value) -> Result<Value> {
    let call: Call = serde_json::from_value(args.clone())?;
    let reported = std::sync::Mutex::new((None::<String>, None::<String>));
    let observer = |id: Option<&str>| {
        authority::require_execution(&host.state)?;
        let mut reported = reported.lock().unwrap();
        if let Some(id) = id {
            valid_session(id)?;
            reported.0 = Some(id.into());
        }
        reported.1.get_or_insert_with(now_text);
        Ok(())
    };
    let access = Arc::new(std::sync::Mutex::new(None));
    let seen = access.clone();
    let remote: crate::llm::implementation::mobile::Observer = Arc::new(move |a| {
        *seen.lock().unwrap() = Some(a);
        Ok(())
    });
    let direction = session(host, &call, &observer, remote).await?;
    let (session_id, started_at) = reported.into_inner().unwrap();
    let access = access.lock().unwrap().take();
    Ok(
        json!({"answer":direction.answer,"problem":direction.problem,"failure":direction.failure,"session_id":session_id,"started_at":started_at,"finished_at":now_text(),"remote":access}),
    )
}

/// Why the director's machine gave no answer, as it reported it: its `failure` within the
/// vocabulary; from a machine that predates causes, what its fixed problem says, else
/// `unknown`.
fn reported_failure(reply: &Value, problem: Option<&str>) -> Failure {
    if !reply["failure"].is_null() {
        return Failure::read(&reply["failure"]);
    }
    Failure::of(match problem {
        Some("The workflow director's answer is not the JSON it was asked for.") => {
            Cause::InvalidAnswer
        }
        Some("The workflow director did not answer in time.") => Cause::Timeout,
        _ => Cause::Unknown,
    })
}
/// The text of a director's session that did not run.
fn did_not_run(failure: &Failure) -> String {
    format!("The workflow director did not run. {}", failure.sentence())
}

/// The fixed problems a machine may report, so no other text of it reaches the record.
fn known_problem(problem: &Value) -> Option<&'static str> {
    [
        "The workflow director returned no answer.",
        "The workflow director's answer is not the JSON it was asked for.",
        "The workflow director did not answer in time.",
    ]
    .into_iter()
    .find(|known| problem == *known)
}

/// Ask the director on its machine and record the session on this coordinator.
pub(in crate::app) async fn ask(host: &Arc<Host>, ask: Ask<'_>) -> Result<Answer> {
    let state = &host.state.implementation;
    let own = state.registry.lock().await.id.clone();
    let local = ask.machine == own;
    if local {
        // The shared server runs no agent for another account.
        authority::require_execution(&host.state)?;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let mut item = ask.item.clone();
    item.dates.clear();
    let director = ask.director;
    let purpose = match ask.task {
        crate::llm::implementation::DirectTask::Classify => "classification",
        crate::llm::implementation::DirectTask::Summarize => "summary",
        crate::llm::implementation::DirectTask::Supervise => "supervision",
        crate::llm::implementation::DirectTask::Audit => "audit",
    };
    let (named, texted) = if local {
        (true, true)
    } else {
        state
            .registry
            .lock()
            .await
            .machines
            .iter()
            .find(|m| m.id == ask.machine)
            .map_or((false, false), |m| (m.session_names, m.prompt_texts))
    };
    let texts = crate::agent_prompts::Texts::new(&host.state.settings.map()?.agent_prompts);
    let record = json!({"id":id,"machine":ask.machine,"purpose":purpose,"project":item.project,"kind":item.kind,"provider":director.provider,"model":director.model,"effort":director.effort,"state":"generating","created_at":now_text(),"started_at":null,"finished_at":null,"session_id":null,"item":item,"output":"","permission_mode":crate::llm::implementation::permission_mode(&director.provider, true)});
    state.db.request_insert(&record)?;
    let call = Call {
        task: ask.task,
        id: id.clone(),
        provider: director.provider.clone(),
        model: director.model.clone(),
        effort: director.effort.clone(),
        instructions: ask.instructions.to_owned(),
        input: ask.input,
        item: named.then_some(item.id),
        seconds: ask.seconds,
        texts: if texted {
            ask.task.changes(&texts)
        } else {
            Default::default()
        },
    };
    let result: Result<(Direction, Option<String>, Option<String>)> = if local {
        let observer = |session: Option<&str>| {
            authority::require_execution(&host.state)?;
            state.db.request_update(&id, |record| {
                if let Some(session) = session {
                    valid_session(session)?;
                    record["session_id"] = json!(session);
                }
                if record["started_at"].is_null() {
                    record["started_at"] = json!(now_text());
                }
                Ok(())
            })
        };
        Box::pin(session(
            host,
            &call,
            &observer,
            super::remote_observer(host, &id),
        ))
        .await
        .map(|direction| (direction, None, None))
    } else {
        async {
            let reply = fleet::call(
                host,
                ask.machine,
                "workflow_direct",
                serde_json::to_value(&call)?,
            )
            .await?;
            // A session ID that is not one is dropped; the answer is still judged.
            let session = reply["session_id"]
                .as_str()
                .filter(|s| valid_session(s).is_ok())
                .map(str::to_owned);
            let started = reply["started_at"]
                .as_str()
                .filter(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok())
                .map(str::to_owned);
            // The session's Remote Control as the machine reported it; a link that is not
            // Claude's own session link is dropped.
            if let Ok(mut access) = serde_json::from_value::<
                crate::llm::implementation::mobile::Access,
            >(reply["remote"].clone())
                && ["connected", "unavailable", "offline"].contains(&access.status.as_str())
            {
                access.url = access
                    .url
                    .and_then(|u| crate::llm::implementation::mobile::claude_url(&u).ok());
                let _ = state.db.remote(&id, access);
            }
            let problem = known_problem(&reply["problem"]);
            let answer = if problem.is_none() {
                reply.get("answer").cloned().filter(|a| !a.is_null())
            } else {
                None
            };
            let failure = answer.is_none().then(|| reported_failure(&reply, problem));
            let direction = Direction {
                problem: problem.or(answer
                    .is_none()
                    .then_some("The workflow director returned no answer.")),
                answer,
                failure,
            };
            Ok((direction, session, started))
        }
        .await
    };
    let finished = |state_name: &str,
                    output: &str,
                    failure: Option<Failure>,
                    session: Option<&str>,
                    started: Option<&str>| {
        let (state_name, output) = (state_name.to_owned(), output.to_owned());
        LocalImplementation::new(&state.db).finish_request(&id, &mut |record| {
            record["state"] = json!(state_name);
            record["output"] = json!(output);
            if let Some(failure) = failure {
                record["failure"] = json!(failure);
            }
            if let Some(session) = session {
                record["session_id"] = json!(session);
            }
            if let Some(started) = started
                && record["started_at"].is_null()
            {
                record["started_at"] = json!(started);
            }
            record["finished_at"] = json!(now_text());
            Ok(())
        })
    };
    match result {
        Ok((direction, session, started)) => {
            finished(
                if direction.answer.is_some() {
                    "done"
                } else {
                    "failed"
                },
                direction.problem.unwrap_or_default(),
                direction.failure,
                session.as_deref(),
                started.as_deref(),
            )?;
            let session_id = state
                .db
                .requests(Some(&id))?
                .pop()
                .and_then(|r| r["session_id"].as_str().map(str::to_owned));
            Ok(Answer {
                run: id,
                session_id,
                direction,
            })
        }
        Err(error) => {
            // Another machine's call that fails carries its cause or never reached it.
            let failure = crate::llm::implementation::of_error(
                &error,
                if local {
                    Cause::Unknown
                } else {
                    Cause::MachineOffline
                },
            );
            let _ = finished("failed", &did_not_run(&failure), Some(failure), None, None);
            // An error that names its cause already says it; any other keeps its own fixed
            // text (this package's) and carries the cause.
            if error.chain().any(|e| e.is::<Failed>()) {
                Err(Failed::wrap(failure, did_not_run(&failure)))
            } else {
                Err(Failed::wrap(failure, error.to_string()))
            }
        }
    }
}

/// Keep code's verdict on the director's answer in the session's record.
pub(in crate::app) fn record(host: &Arc<Host>, run: &str, verdict: Value) -> Result<()> {
    LocalImplementation::new(&host.state.implementation.db).finish_request(run, &mut |record| {
        record["classification"] = verdict.clone();
        Ok(())
    })
}

/// Keep a field about the session (the workflow run a summary is of, the verdict on its
/// output) in its record.
pub(in crate::app) fn annotate(host: &Arc<Host>, run: &str, key: &str, value: Value) -> Result<()> {
    LocalImplementation::new(&host.state.implementation.db).finish_request(run, &mut |record| {
        record[key] = value.clone();
        Ok(())
    })
}
