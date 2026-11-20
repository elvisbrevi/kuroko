// Settings › Prompts: every text the app gives a model, by section. The built-in prompts
// (`agent_prompts` in the snapshot) keep only what the person changed in `map.agent_prompts`; the
// cases' descriptions and instructions are `map.cases`; the workflows' instructions and blocks are
// the person's own texts, edited in Workflows. Edits are unsaved Settings edits: the save bar
// saves them with the profile. To-Do › Guides and instructions edits the agents' instructions too.
import { t, tn } from './i18n.js';
import { $, badge, el } from './dom.js';
import { current } from './state.js';
import { markDirty, showTab } from './nav.js';

/** The sections of the catalog (`agent_prompts::SECTIONS`) and the person's own texts. */
const SECTIONS = [
  ['agents', 'Agent instructions', 'What the implementation agents start from, as To-Do › Guides and instructions shows them.'],
  ['agent_rules', 'Agent rules', 'What code adds after the agents\' instructions: what they may trust and do, and the JSON answer.'],
  ['workflow_director', 'Director and supervisor', 'What code adds to the workflow director\'s and supervisor\'s instructions.'],
  ['workflow_runs', 'Workflow runs', 'How a workflow run\'s context and its review rounds are written.'],
  ['answers', 'Teams answers', 'The rules every answer in Teams is written under.'],
  ['steps', 'Reading a request', 'How the assistant classifies a message, finds what it is about and shortens a reply.'],
  ['activity', 'Activity registration', 'How the day\'s work becomes task drafts and links.'],
  ['cases', 'Cases', 'How the model recognizes each kind of request and how it answers it.'],
  ['workflows', 'Workflow texts', 'Your workflows\' instructions and blocks, edited in Workflows.'],
];

let section = 'agents';
let query = '';
/** Prompts whose editor fold is open, kept across re-renders. */
const open = new Set();

/** Lower case without accents, for the search. */
const plain = text => String(text ?? '').normalize('NFD').replace(/\p{M}/gu, '').toLowerCase();
const matches = (...texts) => !query || texts.some(text => plain(text).includes(plain(query)));

/**
 * One editable text: a row with its state and an editor fold that remembers being open. `name` and
 * `summary` are already translated; `set` keeps a new text, `reset` drops it and returns the default.
 */
function editor(key, { name, summary, placeholders = [], value, set, changed, reset, rows = 8 }) {
  const state = el('span');
  const renderState = () => state.replaceChildren(changed() ? badge(t('Your text'), 'active') : badge(t('Default text'), 'muted'));
  renderState();
  const text = el('textarea', { value, rows, spellcheck: false });
  text.oninput = () => { set(text.value); renderState(); markDirty({ target: text }); };
  const restore = el('button', { type: 'button', class: 'secondary small', textContent: t('Restore the default') });
  restore.onclick = () => { text.value = reset(); renderState(); markDirty({ target: restore }); };
  const values = placeholders.length
    ? el('p', { class: 'hint' }, t('Values code fills in:'), ' ', ...placeholders.flatMap((p, i) => [i ? ', ' : '', el('code', { translate: 'no', textContent: `{{${p}}}` })]))
    : null;
  const edit = el('details', { class: 'fold resource-edit', open: open.has(key) }, el('summary', { textContent: t('Edit the text') }),
    values, el('label', {}, t('Text'), text), el('div', { class: 'actions' }, restore));
  edit.ontoggle = () => { if (edit.open) open.add(key); else open.delete(key); };
  return el('div', { class: 'resource', 'data-prompt': key },
    el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: name }), el('span', { class: 'grow' }), state),
    el('p', { class: 'resource-facts' }, el('span', { textContent: summary })), edit);
}

/** A built-in prompt: the person's change in `map.agent_prompts`, or its default. */
function builtInPrompt(built) {
  const changes = current.map.agent_prompts ??= [];
  const change = () => changes.find(c => c.id === built.id);
  const drop = () => changes.splice(0, changes.length, ...changes.filter(c => c.id !== built.id));
  return editor(`prompt:${built.id}`, {
    name: t(built.name), summary: t(built.summary), placeholders: built.placeholders,
    value: change()?.instructions ?? built.instructions,
    changed: () => Boolean(change()),
    set: value => { drop(); if (value.trim() && value.trim() !== built.instructions.trim()) changes.push({ id: built.id, instructions: value }); },
    reset: () => { drop(); return built.instructions; },
  });
}

/** A case's part (`description` or `instructions`) in `map.cases`, kept only while it differs. */
function casePart(built, part, name, summary) {
  const cases = current.map.cases ??= [];
  const entry = () => cases.find(c => c.id === built.id);
  const set = value => {
    let found = entry();
    if (!found) { found = { id: built.id }; cases.push(found); }
    if (value.trim() && value.trim() !== built[part].trim()) found[part] = value; else delete found[part];
    // A case with nothing left of its own behaves as built.
    if (Object.keys(found).every(key => key === 'id' || (key === 'enabled' && found.enabled !== false))) {
      cases.splice(cases.indexOf(found), 1);
    }
  };
  return editor(`case:${built.id}:${part}`, {
    name, summary, rows: part === 'description' ? 3 : 8,
    value: entry()?.[part] || built[part],
    changed: () => Boolean(entry()?.[part]),
    set,
    reset: () => { set(''); return built[part]; },
  });
}

function caseItems() {
  const items = [];
  for (const built of current.cases ?? []) {
    if (built.description && matches(built.name, built.summary, built.description, built.id)) {
      items.push(casePart(built, 'description', `${t(built.name)} · ${t('How the model recognizes it')}`, t(built.summary)));
    }
    if (built.instructions && matches(built.name, built.summary, built.instructions, built.id)) {
      items.push(casePart(built, 'instructions', `${t(built.name)} · ${t('How it is answered')}`, t(built.summary)));
    }
  }
  return items;
}

/** The person's own workflow texts, read only here: Workflows saves them. */
function workflowItems() {
  const openList = () => showTab('workflows', 'workflows-list');
  const openBlocks = () => showTab('workflows', 'workflows-blocks');
  const row = (title, kind, text, action) => el('div', { class: 'resource' },
    el('div', { class: 'resource-head' }, el('strong', { class: 'title', textContent: title }), el('span', { class: 'grow' }), badge(kind, 'muted')),
    el('p', { class: 'resource-facts prompt-preview' }, el('span', { textContent: text })),
    el('div', { class: 'actions' }, action));
  const button = (label, onclick) => el('button', { type: 'button', class: 'secondary small', textContent: label, onclick });
  const items = [];
  for (const workflow of current.map.workflows ?? []) {
    if (!workflow.instructions?.trim() || !matches(workflow.name, workflow.id, workflow.instructions)) continue;
    items.push(row(workflow.name || workflow.id, t('Workflow'), workflow.instructions, button(t('Edit in Workflows'), openList)));
  }
  for (const block of current.map.workflow_blocks ?? []) {
    if (!block.text?.trim() || !matches(block.name, block.id, block.text)) continue;
    items.push(row(block.name || block.id, t('Block'), block.text, button(t('Edit in Workflows'), openBlocks)));
  }
  return items;
}

function items(name) {
  if (name === 'cases') return caseItems();
  if (name === 'workflows') return workflowItems();
  return (current.agent_prompts ?? [])
    .filter(p => p.section === name && matches(t(p.name), t(p.summary), p.name, p.id, p.instructions))
    .map(builtInPrompt);
}

/** The count of each section's texts the person changed, beside its name. */
function changedIn(name) {
  if (name === 'cases') return (current.map.cases ?? []).reduce((n, c) => n + Number(Boolean(c.description)) + Number(Boolean(c.instructions)), 0);
  if (name === 'workflows') return 0;
  const ids = new Set((current.agent_prompts ?? []).filter(p => p.section === name).map(p => p.id));
  return (current.map.agent_prompts ?? []).filter(c => ids.has(c.id)).length;
}

function renderPrompts() {
  const filters = $('#prompt-sections');
  if (!filters || !current) return;
  filters.replaceChildren(...SECTIONS.map(([name, label]) => {
    const changed = changedIn(name);
    const chip = el('button', { type: 'button', 'data-section': name, 'aria-pressed': String(name === section) }, t(label),
      changed ? el('span', { class: 'count', textContent: String(changed), title: tn(changed, '1 changed', '{n} changed') }) : null);
    chip.onclick = () => { section = name; renderPrompts(); };
    return chip;
  }));
  const [, , about] = SECTIONS.find(([name]) => name === section);
  $('#prompt-section-hint').textContent = t(about);
  const list = items(section);
  $('#prompts').replaceChildren(...(list.length ? list : [el('p', { class: 'hint', textContent: query ? t('No prompt in this section matches the search.') : t('Nothing here yet.') })]));
}

function initPrompts() {
  const search = $('#prompt-search');
  search.oninput = () => { query = search.value.trim(); renderPrompts(); };
}

export { initPrompts, renderPrompts };
