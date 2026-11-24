// Workflows › workflow leads: the agents you name (Claude on this coordinator, with a model and
// effort) that follow the runs of the workflows that name them; for now in shadow, they read and
// record and change nothing. Settings › Workflows lists, edits and removes them
// (`workflow_leads_list`, `workflow_lead_save`, `workflow_lead_remove`); a workflow's editor names
// its lead and its completion criteria (`leadField`, `leadSent`).
import { t, tx, uiLocale } from './i18n.js';
import { invoke } from './control.js';
import { el } from './dom.js';
import { busy, done } from './notices.js';
import { fillAgentChoices } from './settings-models.js';

/** What a workflow's lead may decide by itself (`lead.may`, src/workflows/lead.rs `May`). */
const MAY = [
  { id: 'settle', name: 'Settle children', effect: 'With the branches, pull requests or Wiki pages code verifies they delivered; the run then goes on by itself.' },
  { id: 'close', name: 'Close runs', effect: 'As done by your criteria once every child is settled, also with pull requests still open.' },
  { id: 'wait', name: 'Wait longer', effect: 'For pull requests past your limit, up to 60 days, and move a run ahead in the line.' },
];

/** The longest completion criteria and lead description (src/workflows/lead.rs). */
const MAX_DONE_WHEN = 4000;
const MAX_DESCRIPTION = 2000;
const MAX_NAME = 60;

/** The leads as last read, each `{lead, workflows}`, and whether they were read. @type {any[]} */
let leads = [];
let read = false;
/** What the last save warned about, shown until the next one. @type {string[]} */
let warnings = [];
/** The questions answered here; the host takes an answer at its next tick. @type {Set<string>} */
const answered = new Set();
/** The live talks opened here, by lead: their Remote Control link. @type {Map<string, string>} */
const talks = new Map();

/** A lead ID from its name: lowercase letters, digits and `-`, starting with a letter. */
function slug(name) {
  const plain = name.normalize('NFD').replace(/[̀-ͯ]/g, '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  return (/^[a-z]/.test(plain) ? plain : `lead-${plain}`).slice(0, 64).replace(/-+$/, '') || 'lead';
}

/** Unix seconds in the device's locale. */
function when(seconds) {
  return seconds ? new Date(seconds * 1000).toLocaleString(uiLocale()) : '';
}

/** Reads the leads; the editor and the panel use the same list. */
async function readLeads() {
  leads = (await invoke('workflow_leads_list', {})).leads ?? [];
  read = true;
  return leads;
}

/** A new lead's ID: its name's, with a number when another lead has it. */
function newId(name) {
  const base = slug(name);
  const taken = new Set(leads.map(l => l.lead.id));
  let id = base;
  for (let n = 2; taken.has(id); n++) id = `${base.slice(0, 60)}-${n}`;
  return id;
}

/**
 * Settings › Workflows › Workflow leads: each lead with its agent and the workflows it leads, and a
 * form for a new or changed one. `coordinator` is this computer's machine ID, whose Claude Code
 * models are offered.
 */
async function renderLeads(container, { editable, coordinator }) {
  try { await readLeads(); } catch (error) { container.replaceChildren(el('p', { class: 'callout error', role: 'alert', textContent: String(error?.message ?? error) })); return; }
  // Each lead's latest shift: whether it follows runs now, its last look and what it decided.
  const shifts = new Map();
  try { for (const shift of (await invoke('workflow_lead_shifts', { limit: 50 })).shifts ?? []) if (!shifts.has(shift.lead)) shifts.set(shift.lead, shift); } catch { /* shown without shifts */ }
  let providers = [];
  if (coordinator) {
    try { providers = ((await invoke('implementation_catalog', { machine: coordinator })).providers ?? []).filter(p => p.id === 'claude'); } catch { /* offered as saved */ }
  }
  const draft = { id: '', name: '', description: '', agent: { provider: 'claude', model: '', effort: '' }, enabled: true };
  const name = el('input', { id: 'lead-name', maxLength: MAX_NAME, disabled: !editable, autocomplete: 'off' });
  const description = el('textarea', { id: 'lead-description', rows: 2, maxLength: MAX_DESCRIPTION, disabled: !editable });
  const selects = { provider: el('select', { id: 'lead-provider', disabled: true }), model: el('select', { id: 'lead-model', disabled: !editable }), effort: el('select', { id: 'lead-effort', disabled: !editable }) };
  const enabled = el('input', { type: 'checkbox', id: 'lead-enabled', checked: true, disabled: !editable });
  const fill = lead => {
    Object.assign(draft, { id: '', name: '', description: '', enabled: true }, structuredClone(lead));
    name.value = draft.name; description.value = draft.description ?? ''; enabled.checked = draft.enabled !== false;
    fillAgentChoices(selects, providers, { ...draft.agent, provider: 'claude' }, agent => { draft.agent = { ...agent, provider: 'claude' }; });
  };
  fill(draft);
  const save = el('button', { type: 'button', id: 'lead-save', textContent: t('Save lead'), disabled: !editable });
  save.onclick = () => busy(save, async () => {
    const lead = { id: draft.id || newId(name.value), name: name.value.trim(), description: description.value.trim(), agent: { ...draft.agent, provider: 'claude' }, enabled: enabled.checked };
    const reply = await invoke('workflow_lead_save', { lead });
    warnings = reply.warnings ?? [];
    done(t('Lead saved.'));
    await renderLeads(container, { editable, coordinator });
  });
  // A new lead, after editing another: the form forgets which one it edited.
  const fresh = el('button', { type: 'button', class: 'secondary', id: 'lead-new', textContent: t('New lead'), disabled: !editable });
  fresh.onclick = () => { fill({ agent: { provider: 'claude', model: '', effort: '' } }); name.focus(); };
  const rows = leads.map(({ lead, workflows }) => {
    const edit = el('button', { type: 'button', class: 'secondary small', textContent: t('Edit'), disabled: !editable });
    edit.onclick = () => { fill(lead); name.focus(); };
    const remove = el('button', { type: 'button', class: 'secondary small danger', textContent: t('Remove'), disabled: !editable || workflows.length > 0 });
    remove.onclick = () => {
      if (remove.dataset.armed !== 'true') {
        remove.dataset.armed = 'true'; remove.textContent = t('Confirm removal');
        setTimeout(() => { remove.dataset.armed = ''; remove.textContent = t('Remove'); }, 4000);
        return;
      }
      busy(remove, async () => {
        await invoke('workflow_lead_remove', { lead: lead.id, confirmed: true });
        done(t('Lead removed.'));
        await renderLeads(container, { editable, coordinator });
      });
    };
    const shift = shifts.get(lead.id);
    const following = shift && shift.state !== 'ended';
    const look = el('button', { type: 'button', class: 'secondary small', textContent: t('Look now'), hidden: !following || !editable || shift.talking });
    // Its own conversation, live in the Claude app; a second press confirms (its turns wait meanwhile).
    const talk = el('button', { type: 'button', class: 'secondary small', textContent: shift?.talking ? t('End the talk') : t('Talk to {name}', { name: lead.name }),
      hidden: !following || !editable || !shift.last_wake });
    talk.onclick = () => {
      if (shift.talking) {
        busy(talk, async () => { await invoke('workflow_lead_disconnect', { lead: lead.id }); done(t('{name} goes on with its runs.', { name: lead.name })); await renderLeads(container, { editable, coordinator }); });
        return;
      }
      if (talk.dataset.armed !== 'true') {
        talk.dataset.armed = 'true'; talk.textContent = t('Confirm: its turns wait while you talk');
        setTimeout(() => { talk.dataset.armed = ''; talk.textContent = t('Talk to {name}', { name: lead.name }); }, 4000);
        return;
      }
      busy(talk, async () => {
        // Opened at the press (a phone allows no window after the reply), filled with its link.
        const win = window.open('about:blank', '_blank');
        const reply = await invoke('workflow_lead_connect', { lead: lead.id, confirmed: true }).catch(error => { win?.close(); throw error; });
        if (reply.url) { talks.set(lead.id, reply.url); if (win) win.location.href = reply.url; } else win?.close();
        done(t('{name} waits for you in the Claude app.', { name: lead.name }));
        await renderLeads(container, { editable, coordinator });
      });
    };
    look.onclick = () => busy(look, async () => { await invoke('workflow_lead_wake', { lead: lead.id }); done(t('{name} looks at its runs at its next tick.', { name: lead.name })); });
    const last = shift?.last_wake;
    const state = !shift ? null : following
      ? `${shift.talking ? t('Talking with you') : shift.state === 'failing' ? t('Its turns are failing') : t('Following its runs')}${last ? ` · ${t('last look {time}', { time: when(last.at) })}` : ''} · ${t('{n} decisions', { n: String(shift.decisions ?? 0) })}`
      : t('Last shift ended {time}', { time: when(shift.ended_at) });
    // Its questions for you, each answered once; the answer reaches it at its next turn.
    const questions = (following ? shift.questions ?? [] : []).filter(q => !answered.has(q.id)).map(q => {
      const text = el('textarea', { rows: 2, maxLength: 2000, 'aria-label': t('Your answer'), disabled: !editable });
      const send = el('button', { type: 'button', class: 'small', textContent: t('Answer'), disabled: !editable });
      send.onclick = () => busy(send, async () => {
        if (!text.value.trim()) return;
        await invoke('workflow_lead_answer', { lead: lead.id, question: q.id, answer: text.value.trim() });
        answered.add(q.id);
        done(t('Your answer reaches {name} at its next turn.', { name: lead.name }));
        await renderLeads(container, { editable, coordinator });
      });
      return el('div', { class: 'callout warn lead-question', 'data-question': q.id },
        el('p', {}, el('strong', { textContent: t('{name} asks', { name: lead.name }) }), ' ', el('span', { textContent: q.text })), text, el('div', { class: 'actions' }, send));
    });
    return el('div', { class: 'resource', 'data-lead': lead.id },
      el('div', { class: 'resource-head' }, el('strong', { textContent: lead.name }), el('span', { class: 'grow' }), talk, look, edit, remove),
      state ? el('p', { class: 'hint lead-shift', textContent: state }) : null,
      shift?.talking && talks.has(lead.id) ? el('p', {}, el('a', { href: talks.get(lead.id), target: '_blank', rel: 'noopener', textContent: t('Open the talk in the Claude app') })) : null,
      ...questions,
      el('p', { class: 'resource-facts' }, el('small', { textContent: [`Claude · ${lead.agent.model} · ${lead.agent.effort}`, lead.enabled === false ? t('Off') : null,
        workflows.length ? t('Leads {names}', { names: workflows.join(', ') }) : t('Leads no workflow yet')].filter(Boolean).join(' · ') })),
      lead.description ? el('p', { class: 'hint', textContent: lead.description }) : null);
  });
  container.replaceChildren(
    warnings.length ? el('div', { class: 'callout warn', role: 'status' }, ...warnings.map(w => el('p', { textContent: tx(w) }))) : el('div', {}),
    rows.length ? el('div', { class: 'list' }, ...rows) : el('p', { class: 'hint', textContent: t('No workflow lead yet.') }),
    el('div', { class: 'grid' },
      el('label', {}, t('Name'), ' ', el('small', { textContent: t('How you call it, on every surface.') }), name),
      el('label', {}, t('Model'), selects.model), el('label', {}, t('Effort'), selects.effort)),
    el('label', {}, t('What you want from it'), description),
    el('label', { class: 'check' }, enabled, el('span', { textContent: t('Lead on') })),
    el('div', { class: 'actions' }, save, fresh));
}

/** The editor's fields for a workflow's lead and completion criteria; `changed` runs after each edit. */
function leadField(draft, changed) {
  const choice = el('select', { 'data-lead': 'choice' });
  const options = () => {
    choice.replaceChildren(el('option', { value: '', textContent: t('No lead') }), ...leads.map(({ lead }) => el('option', { value: lead.id, textContent: lead.name })));
    if (draft.lead?.id && !leads.some(({ lead }) => lead.id === draft.lead.id)) choice.append(el('option', { value: draft.lead.id, textContent: draft.lead.id }));
    choice.value = draft.lead?.id ?? '';
  };
  options();
  choice.onchange = () => { draft.lead = choice.value ? { ...draft.lead, id: choice.value } : null; changed(); };
  const criteria = el('textarea', { rows: 3, maxLength: MAX_DONE_WHEN, value: draft.done_when ?? '', 'data-lead': 'done_when', placeholder: t('For example: done once its pull request is open for review.') });
  criteria.oninput = () => { draft.done_when = criteria.value; changed(); };
  // What it decides by itself, out of shadow: each its own box, confirmed when the workflow is saved.
  const may = MAY.map(({ id, name, effect }) => {
    const box = el('input', { type: 'checkbox', 'data-lead-may': id, checked: (draft.lead?.may ?? []).includes(id), disabled: !draft.lead?.id });
    box.onchange = () => { const chosen = new Set(draft.lead?.may ?? []); if (box.checked) chosen.add(id); else chosen.delete(id); draft.lead = { ...draft.lead, may: MAY.map(m => m.id).filter(m => chosen.has(m)) }; changed(); };
    return el('label', { class: 'check' }, box, el('span', {}, t(name), ' ', el('small', { textContent: t(effect) })));
  });
  const applied = el('input', { type: 'checkbox', 'data-lead': 'applied', checked: draft.lead?.shadow === false, disabled: !draft.lead?.id });
  applied.onchange = () => { draft.lead = { ...draft.lead, shadow: !applied.checked }; changed(); };
  choice.addEventListener('change', () => { for (const box of [applied, ...may.map(l => l.querySelector('input'))]) box.disabled = !choice.value; });
  if (!read) readLeads().then(options).catch(() => {});
  return el('fieldset', { class: 'workflow-lead' }, el('legend', { textContent: t('Workflow lead') }),
    el('label', {}, t('Its lead'), ' ', el('small', { textContent: t('Follows its runs with their context; in shadow it reads and records and changes nothing.') }), choice),
    el('label', {}, t('When a run is done'), ' ', el('small', { textContent: t('In your words; its lead reads it.') }), criteria),
    el('p', { class: 'hint' }, t('What it decides by itself once its decisions apply; code checks each one first.')), ...may,
    el('label', { class: 'check' }, applied, el('span', { textContent: t('Apply its decisions (out of shadow)') })));
}

/** What a person confirms about a workflow's lead: shadow, or what it decides by itself. */
function leadLines(lead) {
  const may = MAY.filter(m => (lead.may ?? []).includes(m.id));
  if (lead.shadow !== false || !may.length) return [el('p', { textContent: t('Its lead follows its runs on this coordinator with its own Claude agent and your Claude usage; in shadow it reads them and records what it would decide, and changes nothing.') })];
  return [el('p', { textContent: t('Its lead follows its runs on this coordinator with its own Claude agent and your Claude usage and, by itself, code checking each decision first:') }),
    el('ul', {}, ...may.map(m => el('li', {}, el('strong', { textContent: t(m.name) }), ` · ${t(m.effect)}`)))];
}

/** The lead and completion criteria as the host saves them. */
function leadSent(draft) {
  const sent = {};
  if (draft.lead?.id) {
    sent.lead = { id: draft.lead.id };
    if (draft.lead.may?.length) sent.lead.may = draft.lead.may;
    if (draft.lead.shadow === false) sent.lead.shadow = false;
  }
  if (draft.done_when?.trim()) sent.done_when = draft.done_when.trim();
  return sent;
}

export { leadField, leadLines, leadSent, renderLeads };
