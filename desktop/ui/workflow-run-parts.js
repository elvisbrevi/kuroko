// Workflows › Runs: the parts of one run's detail that only show its record: the stage
// timeline, the dependencies it waits for or waited for, the children with their coding runs,
// branches, pull requests and predecessors, and the outcomes of its acting blocks.
import { t, tn, tx, uiLocale } from './i18n.js';
import { badge, el } from './dom.js';
import { link } from './markdown.js';
import { failureText, runStateLabel } from './run-states.js';
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
      el('div', {}, el('strong', { textContent: runStateLabel(stage.stage) }),
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
      prs.length ? el('ul', { class: 'run-pulls' }, ...prs.map(pr => el('li', {}, link(pr.title ? `!${pr.id} ${pr.title}` : pr.url, pr.url), ' ', badge(tx(pr.status), pr.status === 'completed' ? 'ok' : pr.status === 'abandoned' ? 'warn' : 'info')))) : null);
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

export { blocks, children, dependencies, otherBranches, rounds, timeline, when };
