// Home: the assistant's state, setup steps, status rows and the CLI sessions.
import { transport } from './transport.js';
import { t, tn, tx } from './i18n.js';
import { invoke, rawInvoke } from './control.js';
import { $, el, icon, sleep } from './dom.js';
import { current, live, tab } from './state.js';
import { busy, done, fail } from './notices.js';
import { renderSettingsFlags, showTab } from './nav.js';
import { copy, link } from './markdown.js';
import { startAssistant } from './profile.js';
import { ensureWorkflowSettings, projectsWithoutReadyState, projectsWaitingForDirector } from './settings-workflows.js';
import { runStateLabel } from './run-states.js';
import { openWorkflowRun } from './workflow-runs.js';

// ---------------------------------------------------------------- status and overview

const GENERATED = ['GRAPH_WEBHOOK_SECRET', 'STATE_ENCRYPTION_KEY'];
const providerNames = { codex: 'Codex', claude: 'Claude Code', deepseek: 'DeepSeek', opencode: 'OpenCode' };

function expectedSubscriptions(config) {
  return config.graph.discover_all_chats ? 1 : config.graph.allowed_chats.length + config.graph.channels.length;
}
const entraReady = config => !config.graph.client_id.endsWith('0002') && !config.graph.tenant_id.endsWith('0001');
const urlReady = config => !config.server.public_url.includes('example.com');
function serverHost(config) { try { return new URL(config.server.public_url).host; } catch { return config.server.public_url; } }
const readySources = map => map.resources.filter(r => r.enabled && r.external_processing && r.allowed_conversations.length);
const missingCredentials = credentials => Object.entries(credentials).filter(([name, source]) => source === 'missing' && !GENERATED.includes(name)).map(([name]) => name);
const enabledChain = config => config.llm.chain.length
  ? config.llm.chain.filter(choice => choice.enabled)
  : [{ provider: config.llm.provider, model: config.llm.model }];

function assistantState(snapshot) {
  const config = snapshot.loaded_config ?? snapshot.config;
  const observing = config.policy.dry_run;
  if (!snapshot.running) {
    return { tone: 'muted', pill: t('Assistant stopped'), title: t('Assistant stopped'),
      detail: t('It neither receives nor answers Teams messages. The test chat still works.') };
  }
  let tone = observing ? 'info' : 'ok';
  let teams;
  const expected = expectedSubscriptions(config);
  if (!expected) {
    teams = config.graph.self_chat ? t('Personal chat only') : t('No chats set up to receive');
    if (!config.graph.self_chat) tone = 'warn';
  } else if (snapshot.active_subscriptions >= expected) {
    teams = t('Teams: {n} active subscription(s)', { n: snapshot.active_subscriptions });
  } else {
    const code = snapshot.subscription_issue?.match(/graph_http_(\d{3})/)?.[1];
    teams = code ? t('Teams pending (Graph HTTP {code})', { code }) : t('Teams subscription pending');
    tone = 'warn';
  }
  return {
    tone,
    pill: tone === 'warn' ? t('Running · Teams pending') : observing ? t('Running · observing') : t('Running'),
    title: observing ? t('Running in observation mode') : t('Running and answering'),
    detail: `${observing ? t('Drafts proposals without sending them') : t('Answers in Teams as you')} · ${teams}`,
  };
}

/** Phase the host pushes while the assistant starts or stops (`starting`/`stopping`), else null. */
let transition = null;
const transitions = {
  starting: { tone: 'busy', pill: t('Starting…'), title: t('Starting the assistant…'), detail: t('It will receive Teams messages once it is ready.') },
  stopping: { tone: 'busy', pill: t('Stopping…'), title: t('Stopping the assistant…'), detail: t('It stops receiving Teams messages.') },
};

/** One status row; selecting it opens where that part is changed (`go` = showTab arguments).
 *  Its icon tile takes the tone's color; the value says the same in words. */
function tile(label, value, tone, detail, go, symbol) {
  const row = el('button', { type: 'button', class: `status-row ${tone}` },
    el('span', { class: 'status-icon', 'aria-hidden': 'true' }, icon(symbol)),
    el('span', { class: 'status-value' }, el('span', { class: 'status-label', textContent: label }), el('strong', { textContent: value }),
      el('small', { textContent: detail })),
    icon('arrow'));
  row.onclick = () => showTab(...go);
  return row;
}

let azureDevopsStatus = { state: 'checking', authenticated: false };
let azureDevopsPending = null;
/** Whether Azure DevOps accepts the Azure source's personal access token; there is no Azure sign-in. */
function loadAzureDevopsStatus() {
  if (live?.account?.local_execution === false) return Promise.resolve();
  if (azureDevopsPending) return azureDevopsPending;
  azureDevopsPending = invoke('azure_cli_status').then(result => { azureDevopsStatus = result; })
    .catch(() => { azureDevopsStatus = { state: 'unavailable', authenticated: false }; })
    .finally(() => { azureDevopsPending = null; if (live) renderTiles(live); });
  return azureDevopsPending;
}
function azureDevopsTile() {
  if (live?.account?.local_execution === false) return personalCliTile(t('Azure DevOps'), 'azure-devops-status');
  const state = azureDevopsStatus;
  const value = { authenticated: t('Authenticated'), rejected: t('Token rejected'), not_configured: t('No token'), unavailable: t('Unavailable'), checking: t('Checking…') }[state.state] || t('Unavailable');
  const hint = state.authenticated ? t("Azure DevOps accepts the Azure source's personal access token. Select to refresh.")
    : state.state === 'rejected' ? t("The personal access token expired or was revoked. Store a new one in the Azure source's variable.")
    : state.state === 'not_configured' ? t("Store a personal access token with Work Items (Read & write) in the Azure source's variable.")
    : state.state === 'checking' ? t('Checking the Azure DevOps token…')
    : t('Azure DevOps could not be reached. Select to retry.');
  const fix = ['rejected', 'not_configured'].includes(state.state);
  const row = tile(t('Azure DevOps'), value, state.authenticated ? 'ok' : state.state === 'checking' ? 'muted' : 'warn',
    state.user ? `${state.user} · ${hint}` : hint, ['overview'], 'chip');
  row.id = 'azure-devops-status';
  row.onclick = async () => {
    if (fix) { showTab('settings', 'panel-variables'); return; }
    await busy(row, loadAzureDevopsStatus);
    if (tab === 'overview') $('#azure-devops-status')?.focus({ preventScroll: true });
  };
  return row;
}
let githubCliStatus = { state: 'checking', authenticated: false };
let githubCliPending = null;
function loadGithubCliStatus() {
  if (live?.account?.local_execution === false) return Promise.resolve();
  if (githubCliPending) return githubCliPending;
  githubCliPending = invoke('github_cli_status').then(result => { githubCliStatus = result; })
    .catch(() => { githubCliStatus = { state: 'unavailable', authenticated: false }; })
    .finally(() => { githubCliPending = null; if (live) renderTiles(live); });
  return githubCliPending;
}
function githubCliTile() {
  if (live?.account?.local_execution === false) return personalCliTile(t('GitHub CLI'), 'github-cli-status');
  const state = githubCliStatus;
  const value = { authenticated: t('Authenticated'), not_authenticated: t('Not authenticated'), not_installed: t('Not installed'), unavailable: t('Session unavailable'), checking: t('Checking…') }[state.state] || t('Session unavailable');
  const hint = state.authenticated ? t('GitHub session is valid. Select to refresh.')
    : state.state === 'not_installed' ? t('Install GitHub CLI on the machine running this app.')
    : state.state === 'checking' ? t('Checking the GitHub session on this machine…')
    : t('Sign in with gh auth login on the machine running this app, then refresh GitHub CLI status.');
  const row = tile(t('GitHub CLI'), value, state.authenticated ? 'ok' : state.state === 'checking' ? 'muted' : 'warn',
    state.user ? `${state.user} · ${hint}` : hint, ['overview'], 'chip');
  row.id = 'github-cli-status';
  row.onclick = async () => {
    await busy(row, loadGithubCliStatus);
    if (tab === 'overview') $('#github-cli-status')?.focus({ preventScroll: true });
  };
  return row;
}
function personalCliTile(label, id) {
  const row = tile(label, t('Personal machine required'), 'muted', t('CLI sessions belong on your personal trusted machine.'), ['settings', 'panel-web-account'], 'chip');
  row.id = id;
  return row;
}
setInterval(() => { if (current && tab === 'overview') { loadAzureDevopsStatus(); loadGithubCliStatus(); } }, 60000);

/** Workflows on Home: listed once some exist; a project that starts by itself needs a ready state. */
function workflowTiles(s) {
  const workflows = s.map.workflows ?? [];
  if (!workflows.length) return [];
  ensureWorkflowSettings();
  const missing = projectsWithoutReadyState();
  if (missing?.length) {
    return [tile(t('Workflows'), t('Ready state missing'), 'warn', t('Choose when items of {projects} start by themselves', { projects: missing.join(', ') }), ['settings', 'panel-workflow-ready'], 'flow')];
  }
  const waiting = projectsWaitingForDirector();
  if (waiting?.length) {
    return [tile(t('Workflows'), t('Director missing'), 'warn', t('Several workflows of {projects} start by themselves: choose a workflow director so it can pick one', { projects: waiting.join(', ') }), ['settings', 'panel-workflow-director'], 'flow')];
  }
  const enabled = workflows.filter(w => w.enabled).length;
  return [tile(t('Workflows'), tn(enabled, '1 enabled', '{n} enabled'), enabled ? 'ok' : 'muted',
    missing ? t('Every project that starts by itself has a ready state') : t('Ready states not read yet'), ['workflows'], 'flow')];
}

/**
 * The runs that wait on the user, one row each with the item's verified link (Workflows opens
 * them), then what the workflow leads need: a question to answer, or a lead that is not answering.
 */
function renderWorkflowAttention(s) {
  const runs = s.workflow_attention ?? [];
  const leads = s.lead_attention ?? [];
  $('#workflow-attention-panel').hidden = !runs.length && !leads.length;
  $('#workflow-attention').replaceChildren(...runs.map(run => el('div', { class: 'status-row warn', 'data-run': run.id },
    el('span', { class: 'status-icon', 'aria-hidden': 'true' }, icon('flow')),
    el('span', { class: 'status-value' }, el('span', { class: 'status-label', textContent: runStateLabel(run.state) }),
      el('strong', { textContent: run.item.title }),
      ...(run.attention ? [el('small', { textContent: tx(run.attention).slice(0, 240) })] : [])),
    link(`#${run.item.id}`, run.item.url), el('button', { type: 'button', class: 'secondary small run-open', textContent: t('Open run'), onclick: () => openWorkflowRun(run.id) }))),
  ...leads.map(item => el('div', { class: 'status-row warn', 'data-lead-attention': item.question ?? item.lead },
    el('span', { class: 'status-icon', 'aria-hidden': 'true' }, icon('flow')),
    el('span', { class: 'status-value' }, el('span', { class: 'status-label', textContent: item.kind === 'question' ? t('Question from {name}', { name: item.name }) : t('{name} is not answering', { name: item.name }) }),
      el('strong', { textContent: item.text.slice(0, 240) })),
    el('button', { type: 'button', class: 'secondary small', textContent: item.kind === 'question' ? t('Answer') : t('Open leads'), onclick: () => showTab('settings', 'panel-workflow-leads') }))));
}

function renderTiles(s) {
  const config = s.config;
  const chain = enabledChain(config);
  const [first, ...rest] = chain;
  const ready = readySources(s.map).length;
  const tunnelManaged = config.server.cloudflare_tunnel || s.tunnel_config.trim();
  let host = t('Public URL missing');
  try { if (urlReady(config)) host = new URL(config.server.public_url).host; } catch { /* shown as missing */ }
  const rows = [
    tile(t('Microsoft account'), s.microsoft_connected ? t('Connected') : t('Not connected'), s.microsoft_connected ? 'ok' : 'warn',
      s.microsoft_connected ? t('Reads and answers as you') : t('Connect it in Settings'), ['settings', 'panel-teams'], 'user'),
    azureDevopsTile(), githubCliTile(),
    tile(t('Mode'), config.policy.dry_run ? t('Observation') : t('Sending on'), config.policy.dry_run ? 'info' : 'ok',
      config.policy.dry_run ? t('Sends nothing to Teams') : t('Answers in Teams'), ['settings', 'panel-behavior'], 'eye'),
    tile(t('Reception'), config.graph.discover_all_chats ? t('All my chats') : t('{n} chat(s)', { n: config.graph.allowed_chats.length }),
      s.running && s.active_subscriptions ? 'ok' : 'muted',
      s.running ? t('{n} active subscription(s)', { n: s.active_subscriptions }) : t('Inactive while stopped'), ['settings', 'panel-teams'], 'antenna'),
    tile(t('Tunnel'), tunnelManaged ? (s.tunnel_running ? t('Running') : t('Stopped')) : t('External'), tunnelManaged && s.tunnel_running ? 'ok' : 'muted', host, ['settings', 'panel-server'], 'globe'),
    tile(t('Models'), first ? `${providerNames[first.provider] || first.provider}` : t('None active'), first ? 'ok' : 'warn',
      first ? `${first.model}${rest.length ? ` · ${t('fallback')}: ${rest.map(c => providerNames[c.provider] || c.provider).join(' → ')}` : ` · ${t('no fallback')}`}` : t('Turn one on in Settings'), ['settings', 'panel-llm'], 'chip'),
    tile(t('Sources'), t('{ready} of {total} ready', { ready, total: s.map.resources.length }), ready ? 'ok' : 'warn', t('Enabled, authorized and with an audience'), ['knowledge'], 'book'),
    tile(t('Personal chat'), config.graph.self_chat ? t('Enabled') : t('Disabled'), config.graph.self_chat ? 'ok' : 'muted',
      config.graph.self_chat ? t('Ask it from your Teams notes') : t('Optional'), ['settings', 'panel-self-chat'], 'chat'),
    ...workflowTiles(s),
  ];
  // What needs attention stays in sight; the parts in order fold into one line that opens them.
  const attention = rows.filter(row => row.classList.contains('warn') || row.classList.contains('error'));
  const fine = rows.filter(row => !attention.includes(row));
  $('#tiles').replaceChildren(...attention);
  $('#tiles').hidden = !attention.length;
  renderWorkflowAttention(s);
  $('#tiles-fine').replaceChildren(...fine);
  $('#status-more').hidden = !fine.length;
  $('#status-more-summary').textContent = attention.length
    ? tn(fine.length, '{n} more part in order', '{n} more parts in order')
    : tn(fine.length, 'Everything in order: {n} part', 'Everything in order: {n} parts');
}

function renderSteps(s) {
  const config = s.config;
  const missing = missingCredentials(s.credentials);
  const steps = [
    { done: !missing.length, title: t('Variables'), text: missing.length ? t('Missing: {names}.', { names: missing.join(', ') }) : t('The required variables are stored.'), go: ['settings', missing.length ? 'panel-consumers' : 'panel-variables'] },
    { done: enabledChain(config).length > 0, title: t('Language model'), text: t('Turn on and order Codex, Claude Code, OpenCode or DeepSeek.'), go: ['settings', 'panel-llm'] },
    { done: entraReady(config), title: t('Entra registration'), text: t('Tenant and client ID of your existing registration.'), go: ['settings', 'panel-teams'] },
    { done: s.microsoft_connected, title: t('Microsoft account'), text: t('Sign in to read and answer as you.'), go: ['settings', 'panel-teams'] },
    { done: urlReady(config), title: t('Public URL and tunnel'), text: t('A stable HTTPS URL that reaches the local port.'), go: ['settings', 'panel-server'] },
    { done: readySources(s.map).length > 0, title: t('Authorized sources'), text: t('At least one source enabled, with external processing and an audience.'), go: ['knowledge'] },
    { done: s.running, title: t('Start the assistant'), text: t('It starts receiving Teams messages.'), start: true },
    { done: !config.policy.dry_run, optional: true, title: t('Turn sending on'), text: t('Review the proposals in Messages and turn observation mode off.'), go: ['settings', 'panel-behavior'] },
  ];
  const required = steps.filter(step => !step.optional);
  const completed = required.filter(step => step.done).length;
  $('#setup-progress').textContent = t('{ready} of {total} ready', { ready: completed, total: required.length });
  $('#setup-bar').style.width = `${Math.round(completed / required.length * 100)}%`;
  const ready = completed === required.length;
  // Once every step, optional ones included, is done the checklist has nothing left to say.
  $('#setup-panel').hidden = steps.every(step => step.done);
  $('#setup-ready').hidden = !ready;
  $('#setup-steps').replaceChildren(...steps.map((step, index) => ({ step, index })).filter(({ step }) => !ready || !step.done).map(({ step, index }) => {
    const mark = el('span', { class: 'step-mark' }, step.done ? icon('check') : String(index + 1));
    const title = el('strong', {}, step.title, step.optional ? el('span', { class: 'optional', textContent: ` · ${t('optional')}` }) : null);
    let action = null;
    if (!step.done && step.go) {
      action = el('button', { class: 'secondary small', textContent: t('Set up') });
      action.onclick = () => showTab(...step.go);
    } else if (!step.done && step.start) {
      action = el('button', { class: 'small', textContent: t('Start') });
      action.onclick = () => startAssistant(action);
    }
    return el('li', { class: `step${step.done ? ' done' : ''}` }, mark, el('div', {}, title, el('p', { textContent: step.text })), action);
  }));
}

function renderLive() {
  const s = live;
  if (!s) return;
  if (s.account) {
    $('#account-identity').hidden = false;
    $('#account-identity').textContent = t('Assistant account: {name}', { name: s.account.username });
    $('#account-execution-limit').hidden = s.account.local_execution !== false;
    if (s.account.shared) {
      $('#panel-web-account').hidden = false;
      $('#web-logout-settings').hidden = !transport.browser;
    }
  }
  const state = transitions[transition] ?? assistantState(s);
  // A running assistant's dot pulses slowly; a starting or stopping one, faster.
  const motion = transition ? '' : s.running ? ' live' : '';
  $('#status').className = `pill ${state.tone}${motion}`;
  $('#status').title = state.detail;
  $('#status span').textContent = state.pill;
  // The hero takes the state's color and the face its expression (style.css).
  $('#hero').className = `panel hero ${state.tone}${motion}`;
  $('#hero-title').textContent = state.title;
  $('#hero-detail').textContent = state.detail;
  $('#version').textContent = `v${s.host_version}`;
  $('#start').hidden = s.running;
  $('#restart').hidden = $('#stop').hidden = !s.running;
  for (const button of [$('#start'), $('#restart'), $('#stop')]) button.disabled = Boolean(transition) || button.classList.contains('busy');
  $('#teams-identity-error').hidden = !s.teams_identity_error;
  $('#teams-identity-error').textContent = s.teams_identity_error ? tx(s.teams_identity_error) : '';
  $('#microsoft-state').className = `badge ${s.microsoft_connected ? 'ok' : 'warn'}`;
  $('#microsoft-state').textContent = s.microsoft_connected ? t('Account connected') : t('Account not connected');
  $('#teams-login').textContent = s.microsoft_connected ? t('Reconnect the account') : t('Connect Microsoft account');
  $('#github-state').className = `badge ${s.github_connected ? 'ok' : 'muted'}`;
  $('#github-state').textContent = s.github_connected ? t('Connected') : t('Not connected');
  $('#github-list').disabled = !s.github_connected;
  $('#github-disconnect').disabled = !s.github_connected;
  $('#self-chat-disable').disabled = !s.config.graph.self_chat;
  // Enabling is the next step only while the personal chat is off.
  $('#self-chat-enable').classList.toggle('secondary', Boolean(s.config.graph.self_chat));
  renderTiles(s);
  renderSteps(s);
  renderSettingsFlags({
    variables: missingCredentials(s.credentials).length > 0,
    teams: !s.microsoft_connected || !entraReady(s.config) || !urlReady(s.config),
    models: !enabledChain(s.config).length,
    workflows: Boolean(projectsWithoutReadyState()?.length),
  });
}

export {
  providerNames, urlReady, serverHost, readySources, loadAzureDevopsStatus, loadGithubCliStatus, renderSteps,
  renderLive,
};
