//! Review rounds: when a reviewer rejects a workflow run's pull request (vote -10) or waits for
//! its author (-5), the agent that coded the child corrects it in the same branch and pull
//! request and sends it back for review; the engine only reads and checks.
//!
//! **Detecting.** While a run waits for its pull requests (`pull_requests`, or
//! `waiting_dependency` with children already coded), each read records every reviewer's vote
//! (`Child::votes`); a vote that turns negative starts a round, one that stays negative does
//! not. With `review_on_threads`, active threads of others with no negative vote, nobody
//! approving and no new comment for 30 minutes start one too. A round past the workflow's
//! `review_rounds` (default 2), or an abandoned pull request, stops the run for a person.
//!
//! **Correcting.** The run moves to `addressing_review`, which takes a slot. The round's
//! implementation run is one more turn of the native session that coded the child (`resume`),
//! when its machine keeps it and continues sessions (`implementation_resume_support`); otherwise
//! a new session gets the whole coding prompt and a handover note written by code from the
//! run's record, and the round says so (`mode: new_session`). The round's request is fixed text
//! with the reviewers, votes and open threads as data: third-party text that never widens the
//! work item or the run's permissions (`allow`, as the run got it). The agent pushes to the
//! same branch, replies in each thread, asks each reviewer again (resetting their vote, or a
//! comment naming them when the account cannot), comments a summary of the round and, when
//! the run may change work items, comments on the existing review-support task; it opens no
//! other pull request, completes nothing and approves nothing.
//!
//! **Checking.** After its session, code reads the pull request and its threads: a new commit,
//! a reply in each thread of the round, the summary comment (`[PTA review round N]`) and each
//! reviewer asked again (vote back to zero, or named in a comment). Then the run returns to the
//! stage it came from; otherwise, or when the agent disagrees with an observation
//! (`Review: disagreed`), it stops in `needs_attention` naming what is missing.
use super::engine::{self, Azure};
use super::record::{Child, Implementation, Round, Run, State as Phase};
use super::*;
use crate::ado::workflow::{PullRequest, Thread};
use crate::agent_prompts::Texts;

/// A machine that has not answered for this long, while a round must start, needs a person.
pub(super) const MACHINE_WAIT_SECONDS: i64 = 1800;
/// With `review_on_threads`, threads wait this long without a new comment before a round.
const QUIET_SECONDS: i64 = 1800;
/// The heading the round's summary comment starts with.
pub(super) fn heading(n: u32) -> String {
    format!("[PTA review round {n}]")
}
/// The line a round's session ends its report with.
const REVIEW: &str = "Review:";

fn seconds(timestamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|t| t.timestamp())
}

/// The threads of `pull` a round answers: open (active or pending), written by a person other
/// than the pull request's author.
fn open_threads(pull: &PullRequest, threads: &[Thread]) -> Vec<Thread> {
    threads
        .iter()
        .filter(|t| !t.system && matches!(t.status.as_str(), "active" | "pending"))
        .filter(|t| pull.created_by.as_deref() != Some(t.author_id.as_str()))
        .take(50)
        .cloned()
        .collect()
}

/// What starts a round on one pull request: the reviewers whose vote turned negative since the
/// last read (or, with `on_threads`, the quiet open threads of others while nobody approves).
/// Records every vote read.
fn trigger(
    child: &mut Child,
    pull: &PullRequest,
    threads: Option<&[Thread]>,
    now: i64,
) -> Option<Vec<crate::ado::workflow::Reviewer>> {
    let mut turned = Vec::new();
    for reviewer in &pull.reviewers {
        let key = format!("{}:{}", pull.id, reviewer.id);
        let before = child.votes.insert(key, reviewer.vote);
        if reviewer.vote <= -5 && before.is_none_or(|v| v > -5) {
            turned.push(reviewer.clone());
        }
    }
    if !turned.is_empty() {
        return Some(turned);
    }
    // Threads alone: only when asked, nobody voted against or approved, and quiet for a while.
    let threads = threads?;
    if pull.reviewers.iter().any(|r| r.vote != 0) {
        return None;
    }
    let since = child
        .rounds
        .iter()
        .filter(|r| r.pull_request == pull.id)
        .filter_map(|r| r.finished_at)
        .max()
        .unwrap_or(0);
    let open = open_threads(pull, threads);
    let latest = open
        .iter()
        .flat_map(|t| {
            std::iter::once(t.published_at.as_str()).chain(t.replies.iter().map(|r| r.at.as_str()))
        })
        .filter_map(seconds)
        .max()?;
    let fresh = open
        .iter()
        .any(|t| seconds(&t.published_at).is_some_and(|at| at > since));
    (fresh && now - latest >= QUIET_SECONDS).then(Vec::new)
}

/// While a run waits for its pull requests: start a round when a reviewer asked for changes
/// (none when nothing starts one). Called after the pull requests were read.
pub(super) async fn detect(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    now: i64,
) -> Option<bool> {
    let delivery = run.definition.as_ref()?.delivery.clone();
    let on_threads = delivery.review_on_threads;
    for index in 0..run.children.len() {
        if run.children[index].outcome.as_deref() != Some("done")
            || run.children[index].open_round().is_some()
        {
            continue;
        }
        for pull in run.children[index].pull_requests.clone() {
            if pull.status != "active" {
                continue;
            }
            let threads = if on_threads {
                azure
                    .threads(&host.state, run, &pull.repository, pull.id)
                    .await
                    .ok()
            } else {
                None
            };
            let Some(reviewers) = trigger(&mut run.children[index], &pull, threads.as_deref(), now)
            else {
                continue;
            };
            if delivery.review_rounds == 0 {
                // Rounds are off: the votes are recorded and nothing starts.
                continue;
            }
            let done = run.children[index]
                .rounds
                .iter()
                .filter(|r| r.pull_request == pull.id && r.kind == "review")
                .count() as u32;
            let names = reviewers
                .iter()
                .map(|r| format!("{} ({})", r.name, vote(r.vote)))
                .collect::<Vec<_>>()
                .join(", ");
            if done >= delivery.review_rounds {
                run.stop(
                    Phase::NeedsAttention,
                    now,
                    format!(
                        "Pull request {} was sent back for review {done} times, the most this workflow allows, and {} asked for changes again. Look at it with the reviewer; resume to wait for it again.",
                        pull.url,
                        if names.is_empty() { "a reviewer's threads".to_owned() } else { names }
                    ),
                );
                run.cause = Some("review_limit".into());
                return Some(true);
            }
            let threads = match threads {
                Some(threads) => threads,
                None => azure
                    .threads(&host.state, run, &pull.repository, pull.id)
                    .await
                    .unwrap_or_default(),
            };
            let branch = run.children[index]
                .delivered()
                .into_iter()
                .find(|(r, _)| r.id.eq_ignore_ascii_case(&pull.repository))
                .map(|(_, b)| b)
                .unwrap_or_default();
            let round = Round {
                kind: "review".into(),
                files: Vec::new(),
                n: done + 1,
                pull_request: pull.id,
                url: pull.url.clone(),
                repository: pull.repository.to_lowercase(),
                branch,
                reviewers,
                threads: open_threads(&pull, &threads),
                source_commit: pull.source_commit.clone(),
                started_at: now,
                coding: None,
                mode: None,
                return_to: run.state,
                outcome: None,
                finished_at: None,
                requested: Vec::new(),
                missing: Vec::new(),
            };
            run.children[index].rounds.push(round);
            run.advance(
                Phase::AddressingReview,
                now,
                &format!("review round {} of pull request !{}", done + 1, pull.id),
            );
            return Some(true);
        }
    }
    None
}

fn vote(vote: i32) -> &'static str {
    match vote {
        10 => "approved",
        5 => "approved with suggestions",
        -5 => "waiting for the author",
        -10 => "rejected",
        _ => "no vote",
    }
}

/// The child with the round in progress, and that round.
fn open(run: &Run) -> Option<(usize, Round)> {
    run.children
        .iter()
        .enumerate()
        .find_map(|(i, c)| c.open_round().map(|r| (i, r.clone())))
}

fn set_round(run: &mut Run, index: usize, round: Round) {
    if let Some(last) = run.children[index].rounds.last_mut() {
        *last = round;
    }
}

/// The request of a conflict round: update the branch with its target and resolve the
/// conflicts, nothing else (`reviews.conflict`).
fn conflict_request(child: &Child, round: &Round, target: &str, texts: &Texts) -> String {
    let files = if round.files.is_empty() {
        String::new()
    } else {
        texts.render("reviews.files", &[("files", &round.files.join(", "))])
    };
    let text = texts.render(
        "reviews.conflict",
        &[
            ("n", &round.n.to_string()),
            ("url", &round.url),
            ("kind", &child.kind),
            ("id", &child.id.to_string()),
            ("target", target),
            ("files", &files),
            ("branch", &round.branch),
            ("heading", &heading(round.n)),
        ],
    );
    format!("{text}\n")
}

/// The request a round's session gets (`reviews.round`): the user's text, with the reviewers,
/// the votes and the open threads as data, then the steps.
pub(super) fn request(
    run: &Run,
    child: &Child,
    round: &Round,
    pull: Option<&PullRequest>,
    texts: &Texts,
) -> String {
    let target = pull
        .and_then(|p| p.target.as_deref())
        .unwrap_or("refs/heads/main")
        .trim_start_matches("refs/heads/")
        .to_owned();
    if round.kind == "conflict" {
        return conflict_request(child, round, &target, texts);
    }
    let data = json!({
        "pull_request": {"id": round.pull_request, "url": round.url, "repository_id": round.repository,
            "branch": round.branch, "target": target},
        "reviewers": round.reviewers.iter().map(|r| json!({"id": r.id, "name": r.name, "vote": r.vote, "meaning": vote(r.vote)})).collect::<Vec<_>>(),
        "threads": round.threads.iter().map(|t| json!({"id": t.id, "file": t.file, "line": t.line,
            "author": t.author, "text": t.text})).collect::<Vec<_>>(),
    });
    let support = run
        .definition
        .as_ref()
        .is_some_and(|d| d.allow.contains(&crate::workflows::Allow::WorkItems));
    let support = if support {
        format!("{}\n", texts.get("reviews.support"))
    } else {
        String::new()
    };
    let text = texts.render(
        "reviews.round",
        &[
            ("n", &round.n.to_string()),
            ("kind", &child.kind),
            ("id", &child.id.to_string()),
            ("url", &child.url),
            ("data", &data.to_string()),
            ("branch", &round.branch),
            ("target", &target),
            ("repository", &round.repository),
            ("pr", &round.pull_request.to_string()),
            ("heading", &heading(round.n)),
            ("support", &support),
        ],
    );
    format!("{text}\n")
}

/// The handover a new session gets when the session that coded the child is gone: what the run's
/// record says, written by code (`reviews.handover`, `reviews.handover_report`).
fn handover(round: &Round, report: Option<&str>, texts: &Texts) -> String {
    let mut note = format!(
        "\n{}",
        texts.render(
            "reviews.handover",
            &[("branch", &round.branch), ("url", &round.url)]
        )
    );
    if let Some(report) = report.filter(|r| !r.trim().is_empty()) {
        let tail: String = {
            let chars: Vec<char> = report.chars().collect();
            chars[chars.len().saturating_sub(2000)..].iter().collect()
        };
        note.push_str(&format!("\n{}\n", texts.get("reviews.handover_report")));
        note.push_str(&tail);
    }
    note.push('\n');
    note
}

/// One step of a run in `addressing_review`: launch the round's session, follow it, then check
/// what it did and return to the stage it came from; returns whether the run changed.
pub(super) async fn step(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    workflow: &crate::workflows::Workflow,
    now: i64,
    paused: bool,
) -> std::result::Result<bool, String> {
    let Some((index, mut round)) = open(run) else {
        let back = run
            .children
            .iter()
            .flat_map(|c| c.rounds.last())
            .max_by_key(|r| r.started_at)
            .map_or(Phase::PullRequests, |r| r.return_to);
        run.advance(back, now, "no review round open");
        return Ok(true);
    };
    let child = run.children[index].clone();
    let Some(coding) = round.coding.clone() else {
        if paused {
            return Ok(false);
        }
        // Continue the session that coded the child when its machine keeps it and can continue
        // one; otherwise a new session with a handover note.
        let original = child.coding.clone().ok_or("The child has no coding run.")?;
        let observed = match implementation::observe(host, &original.machine, &original.id).await {
            Ok(observed) => observed,
            Err(error) => {
                if now - round.started_at >= MACHINE_WAIT_SECONDS {
                    return Ok(machine_lost(run, &original.machine, now));
                }
                return engine::wait(run, error.to_string());
            }
        };
        let resumable = implementation::machine_facts(host, &original.machine)
            .await
            .ok()
            .flatten()
            .is_some_and(|m| m.resume);
        let session = observed
            .as_ref()
            .and_then(|o| o["session_id"].as_str())
            .filter(|_| resumable)
            .map(str::to_owned);
        // A round its CLI could not continue stays with a new session.
        if round.mode.is_none() {
            round.mode = Some(
                if session.is_some() {
                    "resumed"
                } else {
                    "new_session"
                }
                .into(),
            );
        }
        round.coding = Some(Implementation {
            id: uuid::Uuid::new_v4().to_string(),
            machine: original.machine.clone(),
            started: false,
            state: None,
            failure: None,
            time_limit_minutes: None,
            lead_backlog: false,
        });
        set_round(run, index, round);
        return Ok(true);
    };
    if !coding.started {
        if paused {
            return Ok(false);
        }
        let detail = match azure
            .item(
                &host.state,
                run.item.source.as_deref(),
                Some(&run.item.project),
                child.id,
            )
            .await
        {
            Ok(detail) => detail,
            Err(error) => return engine::wait(run, error),
        };
        let pulls = azure
            .branch_pulls(
                &host.state,
                run,
                &crate::workflows::Repository {
                    id: round.repository.clone(),
                    name: String::new(),
                },
                &round.branch,
            )
            .await
            .unwrap_or_default();
        let pull = pulls.iter().find(|p| p.id == round.pull_request);
        let texts = engine::prompt_texts(host);
        let mut context = request(run, &child, &round, pull, &texts);
        let original = child.coding.clone().ok_or("The child has no coding run.")?;
        let resume = if round.mode.as_deref() == Some("resumed") {
            implementation::observe(host, &original.machine, &original.id)
                .await
                .ok()
                .flatten()
                .and_then(|o| o["session_id"].as_str().map(str::to_owned))
        } else {
            let report = implementation::observe(host, &original.machine, &original.id)
                .await
                .ok()
                .flatten()
                .and_then(|o| o["output"].as_str().map(str::to_owned));
            context.push_str(&handover(&round, report.as_deref(), &texts));
            None
        };
        let directories = match (&child.directory, run.task()) {
            (Some(directory), false) => vec![directory.clone()],
            _ => match (&child.repository, &child.directory) {
                (Some(repository), Some(directory)) => {
                    engine::task_checkouts(workflow, repository, directory)
                        .into_iter()
                        .map(|(_, d)| d)
                        .collect()
                }
                _ => workflow.checkouts.values().cloned().collect(),
            },
        };
        let limit = engine::time_limit(host, workflow, false);
        let launched = implementation::launch_planned(
            host,
            implementation::Planned {
                id: coding.id.clone(),
                machine: coding.machine.clone(),
                provider: workflow.agent.provider.clone(),
                model: workflow.agent.model.clone(),
                effort: workflow.agent.effort.clone(),
                directories,
                context,
                source: run.item.source.clone(),
                detail,
                planning: false,
                // The same permissions the run was created with.
                allow: engine::runner_workflow(host, run, workflow).allow,
                round: true,
                resume,
                time_limit_minutes: limit,
            },
        )
        .await;
        return match launched {
            Ok(_) => {
                if let Some(c) = round.coding.as_mut() {
                    c.started = true;
                    c.time_limit_minutes = super::engine::recorded_limit(limit);
                }
                set_round(run, index, round);
                run.error = None;
                Ok(true)
            }
            Err(error) if engine::transient(&error.to_string()) => {
                if now - round.started_at >= MACHINE_WAIT_SECONDS
                    && error.to_string().contains("offline")
                {
                    return Ok(machine_lost(run, &coding.machine, now));
                }
                engine::wait(run, error.to_string())
            }
            Err(error) => Err(format!(
                "The review round of {} could not be launched: {error}",
                round.url
            )),
        };
    }
    let observed = match implementation::observe(host, &coding.machine, &coding.id).await {
        Ok(Some(observed)) => observed,
        Ok(None) => {
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "The review round's run {} of {} is not on its machine; resume to start the round again.",
                    coding.id, round.url
                ),
            );
            run.cause = Some("review_failed".into());
            return Ok(true);
        }
        Err(error) => return engine::wait(run, error.to_string()),
    };
    let state = observed["state"].as_str().unwrap_or_default().to_owned();
    if let Some(c) = round.coding.as_mut() {
        c.state = Some(state.clone());
    }
    if state != "running" {
        // The round's agent holds the token too.
        engine::runner_writes(host, azure, run, index, round.started_at, now).await;
    }
    match state.as_str() {
        "running" => {
            set_round(run, index, round);
            Ok(false)
        }
        "done" => {
            let output = observed["output"].as_str().unwrap_or_default().to_owned();
            finish(host, azure, run, index, round, &output, now).await
        }
        other => {
            let failure = engine::observed_failure(&observed);
            // A session its CLI could not continue (gone, or never kept): a new one takes over.
            if round.mode.as_deref() == Some("resumed")
                && failure
                    .is_some_and(|f| f.cause == crate::llm::implementation::Cause::CliExitedAtStart)
            {
                round.mode = Some("new_session".into());
                round.coding = None;
                set_round(run, index, round);
                return Ok(true);
            }
            if let Some(c) = round.coding.as_mut() {
                c.failure = failure;
            }
            round.outcome = Some("failed".into());
            round.finished_at = Some(now);
            set_round(run, index, round.clone());
            run.stop(
                Phase::NeedsAttention,
                now,
                format!(
                    "The review round {} of {} {} Read its record (implementation_runs), then resume to wait for the pull request again.",
                    round.n,
                    round.url,
                    engine::ended(other, failure)
                ),
            );
            run.cause = Some("review_failed".into());
            Ok(true)
        }
    }
}

/// The machine that must continue the round has not answered for half an hour.
fn machine_lost(run: &mut Run, machine: &str, now: i64) -> bool {
    run.stop(
        Phase::NeedsAttention,
        now,
        format!(
            "The review round cannot start: machine {machine}, which holds the session that coded the child, has not answered for {} minutes. Resume once it is back; the round is not counted twice.",
            MACHINE_WAIT_SECONDS / 60
        ),
    );
    run.cause = Some("review_machine".into());
    true
}

/// After the round's session: what its report says, then what code reads (a new commit, a reply
/// in each thread of the round, the summary comment, each reviewer asked again).
async fn finish(
    host: &Arc<Host>,
    azure: &Azure,
    run: &mut Run,
    index: usize,
    mut round: Round,
    output: &str,
    now: i64,
) -> std::result::Result<bool, String> {
    let said = output
        .lines()
        .rev()
        .find_map(|l| {
            l.trim()
                .trim_matches(['«', '»', '*', '`'])
                .strip_prefix(REVIEW)
        })
        .map(|w| w.trim().trim_end_matches('.').to_lowercase());
    let pulls = azure
        .branch_pulls(
            &host.state,
            run,
            &crate::workflows::Repository {
                id: round.repository.clone(),
                name: String::new(),
            },
            &round.branch,
        )
        .await
        .unwrap_or_default();
    let pull = pulls.into_iter().find(|p| p.id == round.pull_request);
    let threads = azure
        .threads(&host.state, run, &round.repository, round.pull_request)
        .await
        .unwrap_or_default();
    let author = pull.as_ref().and_then(|p| p.created_by.clone());
    let after = |at: &str| seconds(at).is_some_and(|t| t >= round.started_at);
    let by_author = |id: &str| author.as_deref().is_none_or(|a| a == id);
    let mut missing = Vec::new();
    if pull.as_ref().and_then(|p| p.source_commit.as_ref()) == round.source_commit.as_ref() {
        missing.push("no new commit in its branch".to_owned());
    }
    if round.kind == "conflict"
        && pull.as_ref().and_then(|p| p.merge_status.as_deref()) == Some("conflicts")
    {
        missing.push("it still conflicts with its target branch".to_owned());
    }
    for asked in &round.threads {
        let replied = threads.iter().find(|t| t.id == asked.id).is_some_and(|t| {
            t.replies
                .iter()
                .any(|r| after(&r.at) && by_author(&r.author_id))
        });
        if !replied {
            missing.push(format!("no reply in thread {}", asked.id));
        }
    }
    let heading = heading(round.n);
    let summary = threads.iter().any(|t| {
        (after(&t.published_at) && t.text.trim_start().starts_with(&heading))
            || t.replies
                .iter()
                .any(|r| after(&r.at) && r.text.trim_start().starts_with(&heading))
    });
    if !summary {
        missing.push(format!("no comment starting with {heading}"));
    }
    round.requested.clear();
    for reviewer in &round.reviewers {
        let vote = pull
            .as_ref()
            .and_then(|p| p.reviewers.iter().find(|r| r.id == reviewer.id))
            .map(|r| r.vote);
        let named = threads.iter().any(|t| {
            (after(&t.published_at) && t.text.contains(&reviewer.name))
                || t.replies
                    .iter()
                    .any(|r| after(&r.at) && r.text.contains(&reviewer.name))
        });
        if vote == Some(0) {
            round
                .requested
                .push(format!("{}: vote_reset", reviewer.name));
        } else if named {
            round.requested.push(format!("{}: comment", reviewer.name));
        } else {
            missing.push(format!(
                "{} was not asked again for a review",
                reviewer.name
            ));
        }
    }
    // Record the votes as read now: one left negative is not a new request for changes.
    if let Some(pull) = &pull {
        for reviewer in &pull.reviewers {
            run.children[index]
                .votes
                .insert(format!("{}:{}", pull.id, reviewer.id), reviewer.vote);
        }
        if let Some(recorded) = run.children[index]
            .pull_requests
            .iter_mut()
            .find(|p| p.id == pull.id && p.repository.eq_ignore_ascii_case(&pull.repository))
        {
            let accepted = recorded.accepted;
            *recorded = pull.clone();
            recorded.accepted = accepted;
        }
    }
    round.finished_at = Some(now);
    if said.as_deref() == Some("disagreed") {
        round.outcome = Some("disagreed".into());
        round.missing = missing;
        set_round(run, index, round.clone());
        run.stop(
            Phase::NeedsAttention,
            now,
            format!(
                "In review round {} of {}, the agent explained in its threads why an observation does not apply. Decide with the reviewer; resume to wait for the pull request again.",
                round.n, round.url
            ),
        );
        run.cause = Some("review_disagreed".into());
        return Ok(true);
    }
    if said.as_deref() != Some("resubmitted") {
        missing.push("its report has no «Review: resubmitted» line".to_owned());
    }
    if !missing.is_empty() {
        round.outcome = Some("unverified".into());
        round.missing = missing.clone();
        set_round(run, index, round.clone());
        run.stop(
            Phase::NeedsAttention,
            now,
            format!(
                "Review round {} of {} ended, but code did not find: {}. Look at the pull request; resume to wait for it again.",
                round.n,
                round.url,
                missing.join("; ")
            ),
        );
        run.cause = Some("review_unverified".into());
        return Ok(true);
    }
    round.outcome = Some("resubmitted".into());
    let back = round.return_to;
    set_round(run, index, round.clone());
    run.advance(back, now, &format!("resubmitted: review round {}", round.n));
    if back == Phase::PullRequests {
        run.waiting_since = run.waiting_since.or(Some(now));
        run.checked_at = None;
    }
    Ok(true)
}

/// Where a run stopped in a review round continues on resume: a round that ended (failed,
/// unverified, disagreed) is accepted as it ended and the run returns to the stage it came from;
/// a round whose session did not run to its end (a restart of the host, a machine gone) starts
/// its session again: the same round, not counted twice.
pub(super) fn resume(run: &mut Run) -> Option<(Phase, Value)> {
    let (index, mut round) = open(run).or_else(|| {
        run.children.iter().enumerate().find_map(|(i, c)| {
            c.rounds
                .last()
                .filter(|r| r.finished_at.is_some_and(|f| f >= run.updated_at - 1))
                .map(|r| (i, r.clone()))
        })
    })?;
    if round.outcome.is_some() {
        let back = round.return_to;
        let url = round.url.clone();
        set_round(run, index, round);
        Some((back, json!({"review_round": url, "accepted": true})))
    } else {
        // Its session never ran to its end (a restart of the host, a machine gone): it starts
        // again, continuing the same session when it can.
        round.coding = None;
        round.mode = None;
        let url = round.url.clone();
        set_round(run, index, round);
        Some((
            Phase::AddressingReview,
            json!({"review_round": url, "started_again": true}),
        ))
    }
}

/// `workflow_run_update_branch {run, confirmed}`: the person asks the agent that coded a child
/// to update its branch after the run stopped on a conflict: a conflict round, the same
/// machinery as a review round (the same session, the same branch and pull request).
pub(super) async fn update_branch(host: &Arc<Host>, args: &Value) -> Result<Value, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        run: String,
        #[serde(default)]
        confirmed: bool,
    }
    let args: Args = serde_json::from_value(args.clone())
        .map_err(|e| invalid(format!("Invalid arguments: {e}")))?;
    runs::require_engine(&host.state)?;
    let found = runs::found(&host.state, &args.run)?;
    let stopped = found
        .conflict
        .clone()
        .filter(|_| found.state == Phase::NeedsAttention && found.cause.as_deref() == Some("conflict"))
        .ok_or_else(|| {
            conflict(format!(
                "Workflow run {} did not stop on a conflict between its pull request and its target branch.",
                found.id
            ))
        })?;
    if !args.confirmed {
        return Err(format!(
            "[confirmation_required] Ask the agent that coded it to update the branch of pull request {} with its target and resolve the conflicts{} in that same branch and pull request, with the permissions the run got? It pushes to that branch; it opens, completes and approves nothing.",
            stopped.url,
            if stopped.files.is_empty() {
                String::new()
            } else {
                format!(" ({})", stopped.files.join(", "))
            }
        ));
    }
    let state = &host.state.workflows;
    let lock = state.lock(&args.run);
    let _guard = lock.lock().await;
    let mut run = runs::found(&host.state, &args.run)?;
    if run.state != Phase::NeedsAttention || run.cause.as_deref() != Some("conflict") {
        return Err(conflict(format!(
            "Workflow run {} no longer waits on its conflict (it is {}).",
            run.id,
            run.state.word()
        )));
    }
    let now = chrono::Utc::now().timestamp();
    let (index, pull) = run
        .children
        .iter()
        .enumerate()
        .find_map(|(i, c)| {
            c.pull_requests
                .iter()
                .find(|p| p.id == stopped.pull_request)
                .map(|p| (i, p.clone()))
        })
        .ok_or_else(|| conflict("The conflicting pull request is not one of the run's."))?;
    let branch = run.children[index]
        .delivered()
        .into_iter()
        .find(|(r, _)| r.id.eq_ignore_ascii_case(&pull.repository))
        .map(|(_, b)| b)
        .unwrap_or_default();
    let n = run.children[index]
        .rounds
        .iter()
        .filter(|r| r.pull_request == pull.id)
        .count() as u32
        + 1;
    run.children[index].rounds.push(Round {
        n,
        kind: "conflict".into(),
        files: stopped.files.clone(),
        pull_request: pull.id,
        url: pull.url.clone(),
        repository: pull.repository.to_lowercase(),
        branch,
        reviewers: Vec::new(),
        threads: Vec::new(),
        source_commit: pull.source_commit.clone(),
        started_at: now,
        coding: None,
        mode: None,
        return_to: Phase::PullRequests,
        outcome: None,
        finished_at: None,
        requested: Vec::new(),
        missing: Vec::new(),
    });
    run.conflict = None;
    run.error = None;
    run.advance(
        Phase::AddressingReview,
        now,
        "update the branch after a conflict",
    );
    state.db.save(&run).map_err(fail)?;
    Ok(json!({"run": run.view(), "round": n}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ado::workflow::Reviewer;

    fn pull(votes: &[(&str, i32)]) -> PullRequest {
        PullRequest {
            id: 7,
            status: "active".into(),
            created_by: Some("author".into()),
            reviewers: votes
                .iter()
                .map(|(id, vote)| Reviewer {
                    id: (*id).into(),
                    name: format!("Reviewer {id}"),
                    vote: *vote,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_vote_that_turns_negative_starts_a_round_and_one_that_stays_so_does_not() {
        let mut child = Child::default();
        assert!(trigger(&mut child, &pull(&[("a", 0)]), None, 0).is_none());
        let started = trigger(&mut child, &pull(&[("a", -10)]), None, 0).unwrap();
        assert_eq!(started[0].id, "a");
        assert!(trigger(&mut child, &pull(&[("a", -10)]), None, 0).is_none());
        // Reset to no vote and rejected again: a new request.
        assert!(trigger(&mut child, &pull(&[("a", 0)]), None, 0).is_none());
        assert!(trigger(&mut child, &pull(&[("a", -5)]), None, 0).is_some());
        // Approving starts nothing; a first read that already rejects does.
        let mut fresh = Child::default();
        assert!(trigger(&mut fresh, &pull(&[("b", 10)]), None, 0).is_none());
        let mut late = Child::default();
        assert!(trigger(&mut late, &pull(&[("c", -10)]), None, 0).is_some());
    }

    #[test]
    fn open_threads_alone_start_a_round_only_when_quiet_and_nobody_voted() {
        let thread = |author: &str, at: &str| Thread {
            id: 1,
            status: "active".into(),
            author_id: author.into(),
            published_at: at.into(),
            ..Default::default()
        };
        let at = "2026-11-16T10:00:00Z";
        let then = seconds(at).unwrap();
        let mut child = Child::default();
        // Too recent, then quiet: a round.
        assert!(
            trigger(
                &mut child,
                &pull(&[]),
                Some(&[thread("other", at)]),
                then + 60
            )
            .is_none()
        );
        assert!(
            trigger(
                &mut child,
                &pull(&[]),
                Some(&[thread("other", at)]),
                then + QUIET_SECONDS
            )
            .is_some()
        );
        // The author's own thread, or a reviewer who voted, starts nothing.
        assert!(
            trigger(
                &mut child,
                &pull(&[]),
                Some(&[thread("author", at)]),
                then + QUIET_SECONDS
            )
            .is_none()
        );
        assert!(
            trigger(
                &mut child,
                &pull(&[("a", 5)]),
                Some(&[thread("other", at)]),
                then + QUIET_SECONDS
            )
            .is_none()
        );
    }
}
