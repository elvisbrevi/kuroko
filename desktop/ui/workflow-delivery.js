// Workflows › a workflow's delivery settings (`delivery`): how its runs treat a child whose
// predecessor's pull request is not completed, how many of its pull requests may be open per
// repository, and the branch its pull requests go into. Left at their defaults, the workflow is
// saved without them and behaves as before.
import { t } from './i18n.js';
import { el } from './dom.js';

/** The most open pull requests per repository a workflow may limit itself to, and its review rounds (src/workflows.rs). */
const MAX_OPEN = 10;
const DEFAULT_ROUNDS = 2;
const MAX_ROUNDS = 5;

/** The delivery settings as the host saves them: only what differs from the defaults. */
function deliverySent(delivery) {
  const sent = {};
  if (delivery?.dependency_policy === 'stack') sent.dependency_policy = 'stack';
  if (delivery?.max_open_pull_requests > 0) sent.max_open_pull_requests = delivery.max_open_pull_requests;
  if (delivery?.target_branch?.trim()) sent.target_branch = delivery.target_branch.trim();
  if (Number.isInteger(delivery?.review_rounds) && delivery.review_rounds !== DEFAULT_ROUNDS) sent.review_rounds = delivery.review_rounds;
  if (delivery?.review_on_threads) sent.review_on_threads = true;
  return sent;
}

/** The editor's fieldset for `draft.delivery`; `changed` runs after each edit. */
function deliveryField(draft, changed) {
  draft.delivery ??= {};
  const policy = el('select', { 'data-delivery': 'dependency_policy' },
    el('option', { value: 'wait_for_pull_request', textContent: t('Wait until its pull request is completed') }),
    el('option', { value: 'stack', textContent: t('Start from its branch, with a pull request stacked on it') }));
  policy.value = draft.delivery.dependency_policy === 'stack' ? 'stack' : 'wait_for_pull_request';
  policy.onchange = () => { draft.delivery.dependency_policy = policy.value; changed(); };
  const open = el('input', { type: 'number', min: 0, max: MAX_OPEN, step: 1, inputMode: 'numeric', value: String(draft.delivery.max_open_pull_requests ?? 0), 'data-delivery': 'max_open_pull_requests' });
  open.oninput = () => { draft.delivery.max_open_pull_requests = Math.max(0, Math.min(MAX_OPEN, Number.parseInt(open.value, 10) || 0)); changed(); };
  const target = el('input', { value: draft.delivery.target_branch ?? '', maxLength: 250, placeholder: t('Each repository\'s default branch'), spellcheck: false, translate: 'no', 'data-delivery': 'target_branch' });
  target.oninput = () => { draft.delivery.target_branch = target.value; changed(); };
  const rounds = el('input', { type: 'number', min: 0, max: MAX_ROUNDS, step: 1, inputMode: 'numeric', value: String(draft.delivery.review_rounds ?? DEFAULT_ROUNDS), 'data-delivery': 'review_rounds' });
  rounds.oninput = () => { draft.delivery.review_rounds = Math.max(0, Math.min(MAX_ROUNDS, Number.parseInt(rounds.value, 10) || 0)); changed(); };
  const threads = el('input', { type: 'checkbox', checked: Boolean(draft.delivery.review_on_threads), 'data-delivery': 'review_on_threads' });
  threads.onchange = () => { draft.delivery.review_on_threads = threads.checked; changed(); };
  return el('fieldset', { class: 'workflow-delivery' }, el('legend', { textContent: t('Dependencies and pull requests') }),
    el('p', { class: 'hint', textContent: t('A run waits, without taking a slot, for the Predecessor links of its work item and of each child it codes; with nothing set here it waits until their pull requests are completed.') }),
    el('label', {}, t('When a child depends on one whose pull request is open'), policy),
    el('label', {}, t('Open pull requests per repository (0: no limit)'), el('small', { textContent: t('A run whose next child would open one more pull request in that repository waits its turn, though it declares no dependency.') }), open),
    el('label', {}, t('Target branch'), el('small', { textContent: t('Where its pull requests go, when not each repository\'s default branch: a predecessor is met once its pull requests are completed into it.') }), target),
    el('label', {}, t('Review rounds per pull request (0: off)'), el('small', { textContent: t('When a reviewer rejects a pull request or waits for its author, the agent that coded it corrects it in the same pull request and sends it back, at most this many times; one more needs you.') }), rounds),
    el('label', { class: 'check' }, threads, el('span', {}, t('Comment threads alone also start a round'), el('small', { textContent: t('Open threads without a negative vote, after 30 minutes without a new comment and while nobody approved.') }))));
}

export { deliveryField, deliverySent };
