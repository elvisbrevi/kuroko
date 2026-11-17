// Rendering untrusted text: provider rich-text trees, Markdown answers and links, built from
// allowed elements only.
import { transport } from './transport.js';
import { t } from './i18n.js';
import { el } from './dom.js';
import { done, notify } from './notices.js';

function todoSafeUrl(raw, base) {
  try {
    const url = new URL(raw, base);
    return url.protocol === 'https:' && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}
function appendLinkedText(text, parent) {
  let last = 0;
  for (const match of text.matchAll(/https:\/\/[^\s<>"\u0000-\u001f]+/g)) {
    let raw = match[0].replace(/[.,;:!?]+$/, '');
    while (raw.endsWith(')') && (raw.match(/\)/g) || []).length > (raw.match(/\(/g) || []).length) raw = raw.slice(0, -1);
    const url = todoSafeUrl(raw);
    parent.append(text.slice(last, match.index), url ? link(raw, url) : raw);
    last = match.index + raw.length;
  }
  parent.append(text.slice(last));
}
function looksLikeMarkdown(text) {
  return /^(?:#{1,6}\s+\S|\s*[-*+]\s+\S|\s*\d+[.)]\s+\S|\s*```|\|.+\|)/m.test(text);
}
/** Render the core's safe rich-text tree; provider HTML never enters the browser's parser. */
function todoRich(text, format, base, content) {
  if (format !== 'html') {
    if (format === 'markdown' || format === 'plainText' || looksLikeMarkdown(text)) return markdown(text);
    const plain = el('div', { class: 'todo-text' });
    appendLinkedText(text, plain);
    return plain;
  }
  if (!Array.isArray(content)) return el('div', { class: 'todo-text', textContent: text });
  const root = el('div', { class: 'md' });
  const allowed = new Set(['p', 'div', 'span', 'strong', 'b', 'em', 'i', 'u', 's', 'del', 'br', 'ul', 'ol', 'li', 'pre', 'code', 'blockquote', 'hr', 'table', 'thead', 'tbody', 'tr', 'th', 'td']);
  let remaining = 12000;
  let clipped = false;
  function rebuild(node, parent, depth = 0) {
    if (--remaining < 0 || depth > 40) { clipped = true; return; }
    if (node.tag === 'notice') { clipped = true; return; }
    if (node.tag === 'text') { appendLinkedText(node.text || '', parent); return; }
    if (node.tag === 'a' || node.tag === 'img') {
      const url = node.url && todoSafeUrl(node.url, base);
      const label = node.tag === 'img' ? node.text || t('View image') : richLabel(node) || url || '';
      parent.append(url ? link(label, url) : label);
      return;
    }
    const tag = /^h[1-6]$/.test(node.tag) ? 'h3' : allowed.has(node.tag) ? node.tag : null;
    if (!tag) return;
    if (tag === 'pre' && !node.children?.some(child => child.tag === 'code') && looksLikeMarkdown(richLabel(node))) {
      parent.append(markdown(richLabel(node)));
      return;
    }
    if (tag === 'code') { parent.append(el(tag, { textContent: richLabel(node) })); return; }
    if (tag === 'pre') { parent.append(el('pre', {}, el('code', { textContent: richLabel(node) }))); return; }
    const safe = el(tag);
    for (const child of node.children || []) rebuild(child, safe, depth + 1);
    parent.append(tag === 'table' ? el('div', { class: 'table' }, safe) : safe);
  }
  const richLabel = node => (node.text || '') + (node.children || []).map(richLabel).join('');
  for (const node of content) rebuild(node, root);
  if (clipped) root.append(el('p', { class: 'hint', textContent: t('This text is too long to display completely. Open the original item for the full content.') }));
  return root;
}

async function copy(text) {
  try { await navigator.clipboard.writeText(text); return true; } catch { /* fall back below */ }
  const area = el('textarea', { value: text, class: 'sr-only', readOnly: true });
  document.body.append(area);
  area.select();
  let copied = false;
  try { copied = document.execCommand('copy'); } catch { copied = false; }
  area.remove();
  return copied;
}

/** In the browser a link opens in a new tab; the app window never navigates, so there it copies the URL. */
function link(label, url) {
  if (transport.browser) return el('a', { class: 'ref', href: url, target: '_blank', rel: 'noopener noreferrer', textContent: label });
  const button = el('button', { type: 'button', class: 'ref', textContent: label, title: t('Copy link: {url}', { url }) });
  button.onclick = async () => {
    if (await copy(url)) done(t('Link copied: {url}', { url }));
    else notify(t('Link: {url}', { url }));
  };
  return button;
}

const INLINE = new RegExp([
  /\\([!-\/:-@[-`{-~])/, // escaped punctuation
  /`([^`]+)`/,
  /\*\*\*(\S(?:.*?\S)?)\*\*\*/,
  /\*\*(\S(?:.*?\S)?)\*\*|(?<!\w)__(\S(?:.*?\S)?)__(?!\w)/,
  /~~(\S(?:.*?\S)?)~~/,
  /\*([^\s*](?:[^*]*?[^\s*])?)\*|(?<!\w)_([^\s_](?:[^_]*?[^\s_])?)_(?!\w)/,
  /\{(red|orange|green|blue|gray):([^{}]*\S[^{}]*)\}/,
  /\[([^\]]+)\]\((https:\/\/[^\s)]+)\)/,
  /(https:\/\/[^\s<>"\u0000-\u001f]+)/,
].map(part => part.source).join('|'), 'g');

function inline(text, parent) {
  const wrap = (tag, body, props) => { const node = el(tag, props); inline(body, node); return node; };
  let last = 0;
  for (const match of text.matchAll(INLINE)) {
    if (match.index > last) appendLinkedText(text.slice(last, match.index), parent);
    const [, escaped, code, both, bold, underBold, strike, italic, underItalic, tone, toned, label, url, bare] = match;
    if (escaped !== undefined) parent.append(escaped);
    else if (code !== undefined) parent.append(el('code', { textContent: code }));
    else if (both !== undefined) parent.append(el('strong', {}, wrap('em', both)));
    else if ((bold ?? underBold) !== undefined) parent.append(wrap('strong', bold ?? underBold));
    else if (strike !== undefined) parent.append(wrap('s', strike));
    else if ((italic ?? underItalic) !== undefined) parent.append(wrap('em', italic ?? underItalic));
    else if (tone !== undefined) parent.append(wrap('span', toned.trim(), { class: `tone-${tone}` }));
    else if (bare !== undefined) appendLinkedText(bare, parent);
    else parent.append(todoSafeUrl(url) ? link(label, url) : label);
    last = match.index + match[0].length;
  }
  if (last < text.length) appendLinkedText(text.slice(last), parent);
}

const LIST_ITEM = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;
const FENCE = /^\s*```/;
const RULE = /^([-*_])(?:\s*\1){2,}\s*$/;
const HEADING = /^(#{1,6})\s+(\S.*)$/;
const QUOTE = /^\s*>\s?(.*)$/;

/** Cells of a pipe-table row: `|` separates them unless escaped or inside a code span; the outer pipes are optional. */
function cells(row) {
  const text = row.trim().replace(/^\|/, '');
  const out = [''];
  let code = false;
  for (let k = 0; k < text.length; k++) {
    if (text[k] === '\\' && text[k + 1] === '|') { out[out.length - 1] += '|'; k++; }
    else if (text[k] === '|' && !code) out.push('');
    else { code = code !== (text[k] === '`'); out[out.length - 1] += text[k]; }
  }
  if (text.endsWith('|') && out[out.length - 1] === '') out.pop();
  return out.map(cell => cell.trim());
}
const isTable = (lines, i) => lines[i].includes('|') && Boolean(lines[i + 1]?.includes('|'))
  && cells(lines[i + 1]).length === cells(lines[i]).length && cells(lines[i + 1]).every(cell => /^:?-+:?$/.test(cell));
// Lines that open a block of their own end the paragraph or list before them.
const opensBlock = (lines, i, quotes) => FENCE.test(lines[i]) || RULE.test(lines[i]) || HEADING.test(lines[i])
  || (quotes && QUOTE.test(lines[i])) || isTable(lines, i);

function table(header, rows) {
  const row = (tag, values) => el('tr', {}, ...header.map((_, k) => { const cell = el(tag); inline(values[k] ?? '', cell); return cell; }));
  return el('div', { class: 'table' }, el('table', {}, el('thead', {}, row('th', header)), el('tbody', {}, ...rows.map(values => row('td', values)))));
}

/* Card blocks: the fenced blocks Teams draws as native card elements (src/adapters/card/blocks.rs), drawn here the same way. */
const TONES = { green: 'ok', orange: 'warn', red: 'error', blue: 'active', gray: 'muted' };
const CALLOUTS = { note: 'ℹ️', tip: '💡', important: '❗', warning: '⚠️', caution: '⛔' };
/** A block's line without its list mark. */
const entry = line => line.trim().replace(/^[-*+]\s+/, '').trim();
/** `Name: value`, split at the first `: `; a line without one is all value. */
function pair(line) {
  const at = line.indexOf(': ');
  if (at >= 0) return [line.slice(0, at).trim(), line.slice(at + 2).trim()];
  return line.endsWith(':') ? [line.slice(0, -1).trim(), ''] : ['', line];
}
/** A number as people write it: `7`, `7.5`, `7,5`, `65%`, `6h`. */
function number(text) {
  let value = text.trim().replace(/[%hH]+$/, '').trim().replace(/ /g, '');
  value = value.includes('.') ? value.replace(/,/g, '') : value.replace(/,/g, '.');
  const parsed = /^-?\d+(?:\.\d+)?$/.test(value) ? Number(value) : NaN;
  return Number.isFinite(parsed) ? parsed : null;
}
/** `done/total` or `NN%`. */
function ratio(text) {
  const value = text.trim();
  const [done, total] = value.includes('/') ? value.split('/', 2).map(number) : value.endsWith('%') ? [number(value), 100] : [null, null];
  return done !== null && total !== null && total > 0 && done >= 0 ? [Math.min(done, total), total] : null;
}
const plainText = text => text.replace(/\[([^\]]+)\]\(https:\/\/[^\s)]+\)/g, '$1').replace(/\{(?:red|orange|green|blue|gray):([^{}]*)\}/g, '$1').replace(/[*`~]/g, '').trim();

function cardBlock(kind, rest, lines) {
  const entries = lines.map(entry).filter(Boolean);
  if (kind === 'facts' && entries.length) {
    return el('dl', { class: 'facts' }, ...entries.flatMap(line => {
      const [name, value] = pair(line);
      const term = el('dt'); inline(name, term);
      const detail = el('dd'); inline(value, detail);
      return [term, detail];
    }));
  }
  if (kind === 'buttons') {
    const items = entries.map(line => {
      const match = line.match(/^\[([^\]]+)\]\((https:\/\/[^\s)]+)\)\s*[—–:-]?\s*(.*)$/);
      if (!match || !todoSafeUrl(match[2])) { const text = el('p'); inline(line, text); return text; }
      const button = link(plainText(match[1]), match[2]);
      return match[3] ? el('div', { class: 'card-button' }, button, el('small', { class: 'hint', textContent: plainText(match[3]) })) : button;
    });
    return items.some(item => item.tagName !== 'P') ? el('div', { class: 'card-buttons' }, ...items) : null;
  }
  if (kind === 'badges' && entries.length) {
    return el('div', { class: 'card-badges' }, ...entries.map(line => {
      const [name, text] = pair(line);
      const tone = text && TONES[name.toLowerCase()];
      return el('span', { class: `badge ${tone || 'muted'}`, textContent: plainText(tone ? text : line) });
    }));
  }
  if (kind === 'progress') {
    const rows = entries.map(line => {
      const [name, amount] = pair(line);
      const parts = ratio(amount) ?? (amount ? null : ratio(name));
      if (!parts) { const text = el('p'); inline(line, text); return text; }
      const label = el('span'); inline(amount ? `${name} · ${amount}` : name, label);
      return el('div', { class: 'card-progress' }, label, el('progress', { max: parts[1], value: parts[0], 'aria-label': plainText(name) }));
    });
    return rows.some(row => row.tagName !== 'P') ? el('div', { class: 'card-progress-list' }, ...rows) : null;
  }
  if (kind === 'chart') {
    const [type = '', ...words] = rest.split(/\s+/);
    const title = plainText(words.join(' '));
    if (!['donut', 'pie', 'bar', 'hbar', 'line', 'gauge'].includes(type.toLowerCase())) return null;
    const points = entries.map(pair);
    const gauge = type.toLowerCase() === 'gauge';
    const values = gauge ? points.map(([name, amount]) => ratio(amount) ?? ratio(name)).filter(Boolean).slice(0, 1)
      : points.map(([name, amount]) => [plainText(name), number(amount)]).filter(([name, value]) => name && value !== null);
    if (!values.length) return null;
    const max = gauge ? values[0][1] : Math.max(...values.map(([, value]) => value), 0) || 1;
    const rows = gauge ? [el('div', { class: 'card-progress' }, el('span', { textContent: `${values[0][0]}/${values[0][1]}` }), el('meter', { min: 0, max, value: values[0][0], 'aria-label': title || 'gauge' }))]
      : values.map(([name, value]) => el('div', { class: 'card-chart-row' }, el('span', { textContent: name }), el('meter', { min: 0, max, value, 'aria-label': name }), el('span', { class: 'hint', textContent: String(value) })));
    return el('figure', { class: 'card-chart' }, ...(title ? [el('figcaption', { textContent: title })] : []), ...rows);
  }
  if (kind === 'details' && entries.length) {
    return el('details', { class: 'card-details' }, el('summary', { textContent: plainText(rest) || t('Details') }), markdown(lines.join('\n')));
  }
  return null;
}

/** Renders the Markdown the assistant writes (the same subset Teams receives as a card, card blocks included), without innerHTML. Quotes are one level deep. */
function markdown(text, quotes = true) {
  const root = el('div', { class: 'md' });
  const source = text.replace(/\r/g, '').split('\n');
  let i = 0;
  while (i < source.length) {
    const line = source[i];
    if (FENCE.test(line)) {
      const [kind = '', ...rest] = line.trim().slice(3).trim().split(/\s+/);
      const code = [];
      i++;
      // A `details` block may hold blocks that name their kind (```buttons…```).
      for (let depth = 0; i < source.length; i++) {
        const nested = source[i].match(/^\s*```\s*(\S*)/);
        if (nested && kind.toLowerCase() === 'details' && nested[1]) depth++;
        else if (nested && depth) depth--;
        else if (nested && !nested[1]) break;
        code.push(source[i]);
      }
      i++;
      root.append(cardBlock(kind.toLowerCase(), rest.join(' '), code) ?? el('pre', {}, el('code', { textContent: code.join('\n') })));
      continue;
    }
    if (!line.trim()) { i++; continue; }
    if (RULE.test(line)) { root.append(el('hr')); i++; continue; }
    const heading = line.match(HEADING);
    if (heading) {
      const node = el(heading[1].length <= 2 ? 'h2' : 'h3');
      inline(heading[2].trim(), node);
      root.append(node);
      i++;
      continue;
    }
    if (quotes && QUOTE.test(line)) {
      const quoted = [];
      while (i < source.length && QUOTE.test(source[i])) quoted.push(source[i++].match(QUOTE)[1]);
      const callout = quoted[0]?.trim().match(/^\[!(note|tip|important|warning|caution)\]$/i)?.[1].toLowerCase();
      if (callout) quoted[0] = `${CALLOUTS[callout]} ${quoted.splice(1, 1)[0] ?? ''}`;
      root.append(el('blockquote', callout ? { class: `callout-${callout}` } : {}, markdown(quoted.join('\n'), false)));
      continue;
    }
    if (isTable(source, i)) {
      const header = cells(source[i]);
      const rows = [];
      i += 2;
      while (i < source.length && source[i].includes('|') && source[i].trim() && !FENCE.test(source[i])) rows.push(cells(source[i++]));
      root.append(table(header, rows));
      continue;
    }
    if (LIST_ITEM.test(line)) {
      const items = [];
      while (i < source.length && source[i].trim() && !opensBlock(source, i, quotes)) {
        const match = source[i].match(LIST_ITEM);
        if (match) items.push({ indent: match[1].length, ordered: /\d/.test(match[2]), start: parseInt(match[2], 10), text: match[3] });
        else if (items.length) items[items.length - 1].text += ` ${source[i].trim()}`;
        i++;
      }
      const stack = [{ indent: -1, list: null, item: null }];
      for (const item of items) {
        while (stack.length > 1 && item.indent < stack[stack.length - 1].indent) stack.pop();
        let top = stack[stack.length - 1];
        if (item.indent > top.indent) {
          const list = el(item.ordered ? 'ol' : 'ul');
          if (item.ordered && item.start > 1) list.start = item.start;
          (top.item ?? root).append(list);
          top = { indent: item.indent, list, item: null };
          stack.push(top);
        }
        const entry = el('li');
        const task = item.text.match(/^\[([ xX])\] (.*)$/);
        if (task) entry.append(task[1] === ' ' ? '☐ ' : '☑ ');
        inline(task ? task[2] : item.text, entry);
        top.list.append(entry);
        top.item = entry;
      }
      continue;
    }
    const paragraph = el('p');
    do {
      if (paragraph.childNodes.length) paragraph.append(el('br'));
      inline(source[i].trim(), paragraph);
      i++;
    } while (i < source.length && source[i].trim() && !LIST_ITEM.test(source[i]) && !opensBlock(source, i, quotes));
    root.append(paragraph);
  }
  return root;
}

export { todoSafeUrl, todoRich, copy, link, markdown };
