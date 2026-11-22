// Building nodes under the CSP (no innerHTML): elements, icons, badges and small form helpers.
import { t } from './i18n.js';

const $ = (selector) => document.querySelector(selector);

function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null) continue;
    if (key === 'class') node.className = value;
    else if (key in node) node[key] = value;
    else node.setAttribute(key, value);
  }
  node.append(...children.flat().filter(child => child !== null && child !== undefined && child !== false));
  return node;
}
function icon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', 'icon');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.append(use);
  return svg;
}
const badge = (text, tone = 'muted') => el('span', { class: `badge ${tone}`, textContent: text });
function field(id, value) { $(id).value = value ?? ''; }
function lines(text) { return text.split('\n').map(s => s.trim()).filter(Boolean); }
function names(text) { return text.split(',').map(s => s.trim()).filter(Boolean); }
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

function rowText(primary, secondary, extra) {
  return el('div', {}, el('strong', {}, primary, extra), el('small', { textContent: secondary }));
}

/**
 * One step of a number field, as if typed: from its value, or from its placeholder (the value an
 * empty field stands for) or its minimum when empty, kept within its bounds.
 * @param {HTMLInputElement} input
 * @param {number} direction 1 or -1
 */
function stepNumber(input, direction) {
  if (input.disabled || input.readOnly) return;
  const by = Number(input.step) > 0 ? Number(input.step) : 1;
  const min = input.min === '' ? -Infinity : Number(input.min);
  const max = input.max === '' ? Infinity : Number(input.max);
  const typed = input.value.trim() === '' ? NaN : Number(input.value);
  const shown = input.placeholder.trim() === '' ? NaN : Number(input.placeholder);
  const floor = Number.isFinite(min) ? min : 0;
  const next = Number.isFinite(typed) ? typed + direction * by
    : Number.isFinite(shown) ? shown + direction * by
      : floor;
  const decimals = (String(input.step).split('.')[1] || '').length;
  input.value = String(Number(Math.min(max, Math.max(min, next)).toFixed(decimals)));
  input.dispatchEvent(new Event('input', { bubbles: true }));
  input.dispatchEvent(new Event('change', { bubbles: true }));
}
/**
 * Gives every number field under `root` its − and + buttons: phones show no spinner, and a tap
 * is easier than typing. The buttons come after the field, so its label still names the field.
 * @param {Element} root
 */
function numberSteppers(root) {
  const inputs = root instanceof HTMLInputElement ? [root] : [...root.querySelectorAll('input')];
  for (const input of inputs) {
    if (input.type !== 'number') continue;
    if (input.parentElement?.classList.contains('stepper')) continue;
    const button = (/** @type {number} */ direction, /** @type {string} */ text, /** @type {string} */ label) =>
      el('button', { type: 'button', class: 'secondary stepper-button', textContent: text, 'aria-label': label, onclick: () => stepNumber(input, direction) });
    const wrap = el('span', { class: 'stepper' });
    input.replaceWith(wrap);
    wrap.append(input, button(-1, '−', t('Decrease')), button(1, '+', t('Increase')));
  }
}

export { $, el, icon, badge, field, lines, names, sleep, rowText, numberSteppers };
