// Workflows › Runs: the parts of one run's detail that show its record: the stage timeline, the
// dependencies it waits for or waited for, the children with their coding runs, branches, pull
// requests and predecessors, the outcomes of its acting blocks, and what a child delivered
// without reporting it, to look for again and to accept.
import { t, tn, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { badge, el } from './dom.js';
import { busy, done, notify } from './notices.js';
import { link } from './markdown.js';
import { failureText, runStateLabel, stageLabel } from './run-states.js';
import { sessionLifeBadge } from './sessions.js';
import { STAGE_NAMES } from './workflow-blocks.js';

const FINAL = ['done', 'cancelled'];

/** Unix seconds in the device's locale. */
function when(seconds) {
  return seconds ? new Date(seconds * 1000).toLocaleString(uiLocale()) : t('Not recorded');
}
/** A task's branches in the workflow's other repositories, as its session reported them. */
function otherBranches(child) {
  return (child.branches ?? []).map(b => t('Branch {name} in {repository}', { name: b.branch, repository: b.repository.name }));
}
function timeline(run) {
  const items = (run.timeline ?? []).map((stage, index, all) => {
    const current = index === all.length - 1 && !FINAL.includes(run.state);
    return el('li', { class: current ? 'current' : '' },
      el('i', { class: `dot ${/fail|needs|interrupt|cancel|uncertain|refused|timed|missing|abandon|permission/.test(stage.outcome ?? '') ? 'warn' : stage.finished_at ? 'ok' : 'info'}`, 'aria-hidden': 'true' }),
      el('div', {}, el('strong', { textContent: stageLabel(stage.stage) }),
        el('small', { textContent: `${when(stage.started_at)}${stage.finished_at ? ` → ${when(stage.finished_at)}` : ''}${stage.outcome ? ` · ${tx(stage.outcome)}` : ''}` })));
  });
  return el('details', { class: 'fold', open: true, id: 'run-timeline' }, el('summary', { textContent: tn(items.length, 'Timeline (1 stage)', 'Timeline ({n} stages)') }),
    items.length ? el('ol', { class: 'run-timeline' }, ...items) : el('p', { class: 'hint', textContent: t('No stage yet.') }));
}

function children(run, sessionButton, sessions) {
  const rows = (run.children ?? []).map(child => {
    const life = sessionLifeBadge(sessions.find(s => s.id === child.coding?.id));
    const state = child.coding?.state ?? (child.coding ? t('Starting') : t('Waiting'));
    const prs = child.pull_requests ?? [];
    return el('div', { class: 'resource run-child', 'data-child': String(child.id) },
      el('div', { class: 'resource-head' }, badge(tx(state), state === 'done' ? 'ok' : ['failed', 'permission_prompt', 'interrupted', 'needs_attention', 'missing'].includes(state) ? 'warn' : 'info'),
        link(`#${child.id} ${child.title}`, child.url), el('span', { class: 'grow' }), life, sessionButton(child.coding)),
      el('p', { class: 'resource-facts' }, el('small', { textContent: [child.repository?.name, child.branch ? t('Branch {name}', { name: child.branch }) : null, ...otherBranches(child), child.outcome ? tx(child.outcome) : null].filter(Boolean).join(' · ') })),
      child.ignored_branches?.length ? el('p', { class: 'hint run-child-ignored', textContent: t('Ignored branch lines for repositories this workflow does not have: {names}', { names: child.ignored_branches.join(', ') }) }) : null,
      child.coding?.failure ? el('p', { class: 'hint run-child-failure', textContent: failureText(child.coding.failure) }) : null,
      child.coding?.stops_at ? el('p', { class: 'hint run-child-limit', textContent: t('Its time limit ({minutes} minutes) stops its session at {time}.', { minutes: String(child.coding.time_limit_minutes), time: when(child.coding.stops_at) }) }) : null,
      prs.length ? el('ul', { class: 'run-pulls' }, ...prs.map(pr => el('li', {}, link(pr.title ? `!${pr.id} ${pr.title}` : pr.url, pr.url), ' ', badge(tx(pr.status), pr.status === 'completed' ? 'ok' : pr.status === 'abandoned' ? 'warn' : 'info')))) : null,
      child.wiki_pages?.length ? el('ul', { class: 'run-pulls run-child-wiki' }, ...child.wiki_pages.map(page => el('li', {}, link(t('Wiki page {path}', { path: page.path }), page.url)))) : null);
  });
  return el('details', { class: 'fold', open: true, id: 'run-children' }, el('summary', { textContent: tn(rows.length, 'Children (1)', 'Children ({n})') }),
    rows.length ? el('div', { class: 'list' }, ...rows) : el('p', { class: 'hint', textContent: run.state === 'plan_review' ? t('The children are created once you confirm the plan.') : t('No child yet.') }));
}

function blocks(run) {
  const names = Object.fromEntries((run.blocks ?? []).map(b => [b.id, b.name]));
  const rows = (run.block_outcomes ?? []).map(o => el('div', { class: 'resource run-block' },
    el('div', { class: 'resource-head' }, badge(tx(o.outcome), ['sent', 'succeeded'].includes(o.outcome) ? 'ok' : ['attempting', 'skipped'].includes(o.outcome) ? 'muted' : ['refused', 'failed', 'timed_out', 'stopped', 'uncertain'].includes(o.outcome) ? 'warn' : 'info'),
      el('strong', { textContent: names[o.block] ?? o.block }), o.stage ? badge(STAGE_NAMES[o.stage] ?? o.stage, 'muted') : null),
    el('p', { class: 'resource-facts' }, el('small', { textContent: [when(o.at), o.reason ? tx(o.reason) : null, o.exit_code != null ? t('Exit code {code}', { code: String(o.exit_code) }) : null, o.duration_ms != null ? t('{n} ms', { n: String(o.duration_ms) }) : null].filter(Boolean).join(' · ') }))));
  if (!rows.length) return el('div', {});
  return el('details', { class: 'fold', id: 'run-blocks' }, el('summary', { textContent: tn(rows.length, 'Block outcomes (1)', 'Block outcomes ({n})') }), el('div', { class: 'list' }, ...rows));
}

/** One predecessor: its link, whether it is met, code's reason and the pull requests that count. */
function predecessor(dependency) {
  const met = dependency.rule === 'skipped' ? badge(t('Skipped'), 'muted') : badge(dependency.satisfied ? t('Met') : t('Not met'), dependency.satisfied ? 'ok' : 'warn');
  const name = `#${dependency.id}${dependency.title ? ` ${dependency.title}` : ''}`;
  return el('li', { 'data-dependency': String(dependency.id) }, met, ' ', dependency.url ? link(name, dependency.url) : el('span', { textContent: name }),
    el('small', { class: 'hint', textContent: ` · ${[dependency.state ? tx(dependency.state) : null, tx(dependency.reason)].filter(Boolean).join(' · ')}` }),
    ...(dependency.pull_requests ?? []).map(pr => el('span', {}, ' ', link(`!${pr.id}`, pr.url), ' ', badge(tx(pr.status), pr.status === 'completed' ? 'ok' : 'info'))));
}

/** The predecessors of the run's item and of each child, and what the run waits for. */
function dependencies(run) {
  const groups = [
    ...(run.dependencies?.length ? [[t('Work item #{id}', { id: String(run.item.id) }), run.dependencies]] : []),
    ...(run.children ?? []).filter(c => c.dependencies?.length && !(run.item.category === 'task' && c.id === run.item.id && run.dependencies?.length)).map(c => [t('Child #{id}', { id: String(c.id) }), c.dependencies]),
  ];
  if (!groups.length && !run.wait) return el('div', {});
  const waiting = run.wait ? el('div', { class: 'callout info', id: 'run-wait' }, el('p', { textContent: tx(run.wait.text) }),
    el('p', { class: 'hint', textContent: t('Waiting since {time}; judged again every watch interval, and the run goes on by itself once they are met.', { time: when(run.wait.since) }) })) : null;
  return el('details', { class: 'fold', open: true, id: 'run-dependencies' }, el('summary', { textContent: t('Dependencies') }), waiting,
    ...groups.map(([label, list]) => el('div', {}, el('h4', { textContent: label }), el('ul', { class: 'run-dependencies' }, ...list.map(predecessor)))));
}

/** A review vote in words (Azure DevOps' -10, -5, 0, 5 and 10). */
function voteText(vote) {
  return ({ '-10': t('rejected'), '-5': t('waiting for the author'), 5: t('approved with suggestions'), 10: t('approved') })[vote] ?? t('no vote');
}

/** The review rounds of the run's pull requests: who asked, how the agent took it and how it ended. */
function rounds(run) {
  const all = (run.children ?? []).flatMap(child => (child.rounds ?? []).map(round => ({ child, round })));
  if (!all.length) return el('div', {});
  const rows = all.map(({ child, round }) => {
    const outcome = round.outcome ?? 'in_progress';
    const tone = outcome === 'resubmitted' ? 'ok' : ['in_progress', 'accepted'].includes(outcome) ? 'info' : 'warn';
    const facts = [
      round.kind === 'conflict' ? t('Update after a conflict') : (round.reviewers ?? []).map(r => `${r.name}: ${voteText(r.vote)}`).join(', '),
      round.mode === 'resumed' ? t('Same agent session') : round.mode === 'new_session' ? t('New session with a handover note') : null,
      (round.threads ?? []).length ? tn(round.threads.length, '1 open thread', '{n} open threads') : null,
      ...(round.requested ?? []).map(r => tx(r)),
      round.finished_at ? when(round.finished_at) : when(round.started_at),
    ].filter(Boolean).join(' · ');
    return el('li', { 'data-round': `${child.id}:${round.pull_request}:${round.n}` },
      badge(outcome === 'in_progress' ? t('In progress') : tx(outcome), tone), ' ',
      link(t('Round {n} of !{id}', { n: String(round.n), id: String(round.pull_request) }), round.url),
      el('small', { class: 'hint', textContent: ` · ${facts}` }),
      (round.missing ?? []).length ? el('p', { class: 'hint run-round-missing', textContent: t('Not found: {what}', { what: round.missing.map(m => tx(m)).join('; ') }) }) : null);
  });
  return el('details', { class: 'fold', open: true, id: 'run-rounds' }, el('summary', { textContent: tn(rows.length, 'Review rounds (1)', 'Review rounds ({n})') }), el('ul', { class: 'run-rounds' }, ...rows));
}

/**
 * For each coded child without a delivery: what code found it delivered without reporting it (the
 * branches you pushed while it coded, their pull requests and those linked to the work item, the
 * Wiki pages you changed), to look for again (`workflow_run_find`) and, on a stopped run, to
 * accept what you tick as its delivery once confirmed (`workflow_run_accept_found`). A repository
 * with several branches has none ticked: one is chosen.
 */
function foundPanels(run, { twice, refresh, resumable }) {
  if (FINAL.includes(run.state)) return [];
  const delivered = child => child.branch || child.branches?.length || child.wiki_pages?.length;
  const coded = child => child.coding?.started && child.coding.state !== 'running';
  return (run.children ?? []).filter(child => coded(child) && !delivered(child)).map(child => {
    const found = child.found;
    const branches = found?.branches ?? [];
    const alone = branch => branches.filter(b => b.repository.id === branch.repository.id).length === 1;
    const picks = [];
    const pick = (label, checked, value) => {
      const box = el('input', { type: 'checkbox', checked, disabled: !resumable });
      picks.push({ box, value });
      return el('label', { class: 'check' }, box, el('span', { textContent: label }));
    };
    const items = [
      ...branches.map(branch => el('li', {},
        pick(t('Branch {name} in {repository}', { name: branch.branch, repository: branch.repository.name }), alone(branch), { branch: { repository: branch.repository.id, branch: branch.branch } }),
        ' ', badge(branch.linked ? t('Linked to the work item') : t('Pushed'), 'muted'), ' ', link(t('Open'), branch.url))),
      ...(found?.pull_requests ?? []).map(pr => el('li', {}, link(pr.title ? `!${pr.id} ${pr.title}` : pr.url, pr.url), ' ',
        badge(tx(pr.status), pr.status === 'completed' ? 'ok' : pr.status === 'abandoned' ? 'warn' : 'info'))),
      ...(found?.wiki_pages ?? []).map(page => el('li', {},
        pick(t('Wiki page {path}', { path: page.path }), true, { page: page.path }), ' ', link(t('Open'), page.url))),
    ];
    const actions = el('div', { class: 'actions' });
    if (picks.length && resumable) {
      const accept = twice(t('Accept as its delivery'), t('Confirm: accept what was found'), async () => {
        const chosen = picks.filter(p => p.box.checked).map(p => p.value);
        if (!chosen.length) { notify(t('Tick what to accept.'), 'warn'); return; }
        await invoke('workflow_run_accept_found', { run: run.id, child: child.id, confirmed: true,
          branches: chosen.filter(c => c.branch).map(c => c.branch), wiki_pages: chosen.filter(c => c.page).map(c => c.page) });
        done(t('Accepted as its delivery; the run goes on by itself.'));
        await refresh();
      });
      accept.id = `run-accept-found-${child.id}`;
      actions.append(accept);
    }
    const look = el('button', { type: 'button', class: 'secondary', id: `run-find-${child.id}`, textContent: found ? t('Look again') : t('Look for what it delivered') });
    look.onclick = () => busy(look, async () => {
      await invoke('workflow_run_find', { run: run.id, child: child.id });
      done(t('Looked again.'));
      await refresh();
    });
    actions.append(look);
    return el('div', { class: 'callout info run-found', id: `run-found-${child.id}` },
      el('p', {}, el('strong', { textContent: t('What #{id} delivered without reporting it', { id: String(child.id) }) })),
      el('p', { class: 'hint', textContent: t('Read with your source\'s read credential while it coded: the branches you pushed to its repositories, their pull requests and those you opened and linked to the work item, and the Wiki pages you changed. Only what you tick becomes its delivery.') }),
      items.length ? el('ul', { class: 'run-pulls run-found-list' }, ...items)
        : el('p', { textContent: found ? t('Nothing was found in its repositories or the Wiki (read {time}).', { time: when(found.read_at) }) : t('Not looked for yet.') }),
      found?.partial ? el('p', { class: 'hint', textContent: t('Some reads failed: what is listed may be incomplete.') }) : null,
      actions);
  });
}

/** A finding's severity, as the person reads it. */
const SEVERITY = { blocking: 'Blocking', important: 'Important', minor: 'Minor' };
/** A finding's state, as the person reads it. */
const FINDING_STATES = { resolved: 'Resolved', dismissed: 'Dismissed', proposed: 'Work item proposed', sending: 'Being created', item_created: 'Work item created', uncertain: 'Not confirmed' };
/**
 * Findings the runs' coding sessions reported (`workflow_findings`): severity, title, detail,
 * evidence and its work item; an open one is resolved or dismissed (`workflow_finding_resolve`,
 * `workflow_finding_dismiss`), which changes only its record.
 */
function findingsPanel(findings, refresh, id, title) {
  if (!findings.length) return el('div', {});
  const rows = findings.map(f => el('div', { class: 'resource run-finding', 'data-finding': f.id },
    el('div', { class: 'resource-head' }, badge(t(SEVERITY[f.severity] ?? f.severity), f.severity === 'blocking' ? 'warn' : f.severity === 'important' ? 'info' : 'muted'),
      el('strong', { textContent: f.title }), el('span', { class: 'grow' }),
      f.state === 'open' ? null : badge(t(FINDING_STATES[f.state] ?? f.state), f.state === 'uncertain' ? 'warn' : 'muted')),
    f.detail ? el('p', { textContent: f.detail }) : null,
    f.evidence ? el('p', { class: 'hint', textContent: t('Evidence: {text}', { text: f.evidence }) }) : null,
    el('p', { class: 'resource-facts' }, el('small', {}, link(`#${f.item}`, f.item_url), ` · ${when(f.created_at)}${f.note ? ` · ${f.note}` : ''}`)),
    // The work item its lead drafted: approved here, created once.
    f.draft ? el('div', { class: 'hint' }, el('p', { textContent: t('Drafted work item: {type} «{title}»', { type: f.draft.type, title: f.draft.title }) }), el('p', { textContent: f.draft.description ?? '' })) : null,
    f.created?.url ? el('p', {}, t('Created as '), link(`#${f.created.id}`, f.created.url)) : null,
    f.state === 'uncertain' ? el('p', { class: 'callout warn', textContent: t('It was sent but not confirmed: look in Azure DevOps whether it exists; it is never sent again.') }) : null,
    f.state === 'proposed' ? el('div', { class: 'actions' }, (() => {
      const approve = el('button', { type: 'button', class: 'small', textContent: t('Approve and create') });
      approve.onclick = () => {
        if (approve.dataset.armed !== 'true') {
          approve.dataset.armed = 'true'; approve.textContent = t('Confirm: create it in Azure DevOps');
          setTimeout(() => { approve.dataset.armed = ''; approve.textContent = t('Approve and create'); }, 4000);
          return;
        }
        busy(approve, async () => { await invoke('workflow_finding_approve', { finding: f.id, seen: f.updated_at, confirmed: true }); done(t('Work item created.')); await refresh(); });
      };
      return approve;
    })()) : null,
    ['open', 'proposed'].includes(f.state) ? el('div', { class: 'actions' }, ...[['workflow_finding_resolve', t('Mark resolved')], ['workflow_finding_dismiss', t('Dismiss')]].map(([method, label]) => {
      const button = el('button', { type: 'button', class: 'secondary small', textContent: label });
      button.onclick = () => busy(button, async () => { await invoke(/** @type {any} */ (method), { finding: f.id }); done(t('Finding updated.')); await refresh(); });
      return button;
    })) : null));
  return el('details', { class: 'fold', open: true, id }, el('summary', { textContent: title }), el('div', { class: 'list' }, ...rows));
}

/** What a decision of a lead means, as the person reads it. */
const DECISIONS = { settle: 'Settle its child', close: 'Close the run', wait: 'Keep waiting', continue: 'Continue its agent session', ask_person: 'Ask you' };
/** The run's state beside a lead's decision, as the host wrote it: `state` or `state in stage`. */
function engineState(engine) {
  const [state, stage] = engine.split(' in ');
  return stage ? t('{state} in {stage}', { state: runStateLabel(state), stage: stageLabel(stage) }) : runStateLabel(state);
}

/** What a journal entry is, as a badge's text. */
const KINDS = { finding: 'Finding', question: 'Question', note: 'Note', summary: 'Summary' };

/**
 * What the run's workflow lead recorded about it (`workflow_lead_shifts` with `run`): its
 * decisions (in shadow, recorded and not applied, beside the run's state then), notes, findings
 * and questions for you with your answers.
 */
function leadPanel(shifts) {
  const entries = shifts.flatMap(s => s.journal ?? []).sort((a, b) => b.at - a.at);
  if (!entries.length) return el('div', {});
  const rows = entries.map(e => el('li', {},
    badge(e.kind === 'decision' ? t(DECISIONS[e.decision] ?? e.decision) : t(KINDS[e.kind] ?? 'Note'), e.kind === 'decision' ? 'info' : e.kind === 'question' && !e.answer ? 'warn' : 'muted'), ' ',
    el('span', { textContent: e.text }),
    e.answer ? el('small', { textContent: ` · ${t('Your answer: {text}', { text: e.answer })}` }) : null,
    e.result === 'applied' ? el('small', { textContent: ` · ${t('its decision was applied')}` }) : e.result?.startsWith('refused: ') ? el('small', { textContent: ` · ${t('refused: {reason}', { reason: e.result.slice(9) })}` }) : null,
    el('small', { textContent: ` · ${when(e.at)}${e.engine ? ` · ${t('the run was {state}', { state: engineState(e.engine) })}` : ''}` })));
  return el('details', { class: 'fold', open: true, id: 'run-lead' }, el('summary', { textContent: t('What its lead said (in shadow: nothing is applied)') }), el('ul', { class: 'run-pulls' }, ...rows));
}

export { blocks, children, dependencies, findingsPanel, foundPanels, leadPanel, otherBranches, rounds, timeline, when };
