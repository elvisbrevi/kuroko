// Settings › General and Microsoft Teams: chat reading and the Teams message style.
import { t } from './i18n.js';
import { $, field } from './dom.js';
import { current } from './state.js';

// ---------------------------------------------------------------- settings

/** The allowed-chats list only applies while not every chat is discovered; its value is kept either way. */
function renderChatList() {
  $('#allowed-chats-field').hidden = $('#discover-chats').checked;
  const all = $('#reading-all-chats').checked;
  $('#reading-personal-chat-field').hidden = all;
  $('#reading-chats-field').hidden = all;
}

// ---------------------------------------------------------------- Teams message style

/** Color combinations for the Teams card: any stripe color, and Teams' theme-aware band and text colors. */
const STYLE_PRESETS = {
  teams: { accent: '#5B5FC7', band: 'accent', label_color: 'default' },
  blue: { accent: '#2F80ED', band: 'accent', label_color: 'accent' },
  green: { accent: '#13A10E', band: 'good', label_color: 'good' },
  amber: { accent: '#CA5010', band: 'warning', label_color: 'warning' },
  red: { accent: '#E74856', band: 'attention', label_color: 'attention' },
  gray: { accent: '#8A8A8A', band: 'emphasis', label_color: 'subtle' },
};
/** `[message_style]` as the host defaults it; the host omits the section while it is unchanged. */
const STYLE_DEFAULT = { label: '', position: 'bottom', ...STYLE_PRESETS.teams, icon: true, emoji: '🤖', band_height: 0, stripe_width: 4, text_size: 'default', label_size: 'small', kuroko: false, animated: false, mark_size: 0, site_link: true, link_underline: false, mark_side: 'left', band_align: 'right', band_padding: 0, hide_label: false, mark_image: '' };
/** The mark's side in pixels for each band text size when no size is set, as the host draws it. */
const MARK_PIXELS = { small: 16, default: 20, medium: 24, large: 28, extralarge: 32 };
const DEFAULT_EMOJI = '🤖';
/** The band's own image as the host keeps it, the side a larger still JPEG is reduced to here,
 * and the largest file the host reduces itself (a PNG or GIF, keeping its animation). */
const MARK_IMAGE_BYTES = 100_000;
const MARK_IMAGE_SIDE = 1024;
const MARK_REDUCED_SIDE = 192;
const MARK_SOURCE_BYTES = 2_800_000;
/** The band's own image, PNG or JPEG in base64 (a GIF until the host saves it as PNG); empty for none. */
let markImage = '';

function renderMessageStyle() {
  const style = { ...STYLE_DEFAULT, ...current.config.message_style };
  field('#style-label', style.label);
  field('#style-position', style.position);
  field('#style-accent', style.accent.toLowerCase());
  field('#style-band', style.band);
  field('#style-label-color', style.label_color);
  $('#style-icon').checked = style.icon;
  $('#style-kuroko').checked = style.kuroko;
  $('#style-hide-label').checked = style.hide_label;
  markImage = style.mark_image || '';
  $('#style-animated').checked = style.animated;
  $('#style-site-link').checked = style.site_link;
  $('#style-link-underline').checked = style.link_underline;
  field('#style-mark-side', style.mark_side);
  field('#style-band-align', style.band_align);
  field('#style-band-padding', style.band_padding || '');
  field('#style-mark-size', style.mark_size || '');
  field('#style-emoji', style.emoji || DEFAULT_EMOJI);
  field('#style-stripe-width', style.stripe_width);
  field('#style-band-height', style.band_height || '');
  field('#style-text-size', style.text_size);
  field('#style-label-size', style.label_size);
  renderStylePreview();
}

/** The combination the color fields match, or custom. */
function stylePreset() {
  const accent = $('#style-accent').value.toUpperCase();
  const match = Object.entries(STYLE_PRESETS).find(([, preset]) =>
    preset.accent === accent && preset.band === $('#style-band').value && preset.label_color === $('#style-label-color').value);
  return match?.[0] ?? 'custom';
}

function renderStylePreview() {
  $('#style-preset').value = stylePreset();
  const preview = $('#style-preview');
  preview.className = `teams-card band-${$('#style-band').value} label-${$('#style-label-color').value} at-${$('#style-position').value} text-${$('#style-text-size').value} label-size-${$('#style-label-size').value} mark-${$('#style-mark-side').value} align-${$('#style-band-align').value}`;
  preview.style.setProperty('--band-padding', `${bandPadding() ?? 0}px`);
  // An empty size follows the band text: the field shows the size that gives.
  const followed = MARK_PIXELS[$('#style-label-size').value] || 20;
  $('#style-mark-size').placeholder = String(followed);
  preview.style.setProperty('--mark-size', `${markSize() || followed}px`);
  preview.style.setProperty('--stripe', $('#style-accent').value);
  preview.style.setProperty('--stripe-width', `${stripeWidth() ?? 4}px`);
  const height = bandHeight();
  if (height) preview.style.setProperty('--band-height', `${height}px`);
  else preview.style.removeProperty('--band-height');
  // The default band text follows the reply language, like what the assistant sends.
  const kuroko = $('#style-kuroko').checked;
  const siteLink = kuroko && $('#style-site-link').checked;
  const linkLook = siteLink && $('#style-link-underline').checked;
  const english = $('#llm-language').value === 'en';
  const hideLabel = $('#style-hide-label').checked;
  $('#style-label-field').hidden = hideLabel;
  $('#style-preview-label').hidden = hideLabel;
  const fallback = kuroko
    ? (english ? 'Message generated by Kuroko' : 'Mensaje generado por Kuroko')
    : (english ? 'AI-generated response' : 'Respuesta generada con IA');
  previewLabel($('#style-label').value.trim() || fallback, linkLook);
  const emoji = $('#style-emoji').value.trim() || DEFAULT_EMOJI;
  $('#style-preview-icon').textContent = emoji;
  // Signed as Kuroko, the app's mark takes the emoji's place; the own image takes either's.
  const icon = $('#style-icon').checked;
  const own = Boolean(markImage) && (kuroko || icon);
  const mark = $('#style-preview-mark');
  mark.hidden = !kuroko && !own;
  mark.src = own ? `data:${imageType(markImage)};base64,${markImage}` : kuroko && $('#style-animated').checked ? 'kuroko-mark.png' : 'favicon.png';
  mark.classList.toggle('own', own);
  $('#style-kuroko-fields').hidden = !kuroko;
  $('#style-link-underline-field').hidden = !siteLink;
  $('#style-mark-size-field').hidden = !kuroko && !own;
  $('#style-preview-icon').hidden = kuroko || own || !icon;
  $('#style-icon-field').hidden = kuroko;
  $('#style-emoji-field').hidden = kuroko || own || !icon;
  $('#style-image-remove').hidden = !markImage;
  const facts = { kind: imageType(markImage).slice(6).toUpperCase(), size: Math.max(1, Math.round(markImage.length * 0.75 / 1000)) };
  $('#style-image-status').textContent = !markImage ? t('No image: the band uses the Kuroko icon or the emoji.')
    : animatedPng(markImage) ? t('Animated {kind} image, {size} KB', facts)
      : t('{kind} image, {size} KB', facts);
  document.querySelectorAll('#style-emoji-choices button').forEach(button => {
    button.setAttribute('aria-pressed', String(button.dataset.emoji === emoji));
  });
}

/** The type of an image in base64, from its first bytes. */
function imageType(data) {
  if (data.startsWith('/9j/')) return 'image/jpeg';
  if (data.startsWith('R0lGOD')) return 'image/gif';
  return 'image/png';
}

/** The kind of image bytes, as the host reads it: PNG, JPEG, GIF or nothing. */
function sniffImage(bytes) {
  const starts = (/** @type {number[]} */ ...head) => head.every((byte, at) => bytes[at] === byte);
  if (starts(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)) return 'png';
  if (starts(0xff, 0xd8, 0xff)) return 'jpeg';
  if (starts(0x47, 0x49, 0x46, 0x38)) return 'gif';
  return null;
}

/** Whether a PNG in base64 is animated: an `acTL` chunk before its image data. */
function animatedPng(data) {
  if (imageType(data) !== 'image/png') return false;
  // The chunks before the image data are near the start: its first 4 KB are enough.
  const bytes = Uint8Array.from(atob(data.slice(0, 5460)), c => c.charCodeAt(0));
  for (let at = 8; at + 8 <= bytes.length;) {
    const length = ((bytes[at] << 24) >>> 0) + (bytes[at + 1] << 16) + (bytes[at + 2] << 8) + bytes[at + 3];
    const type = String.fromCharCode(...bytes.subarray(at + 4, at + 8));
    if (type === 'acTL') return true;
    if (type === 'IDAT') return false;
    at += 12 + length;
  }
  return false;
}

/** Bytes in base64, in slices so a large image does not overflow the call stack. */
function toBase64(bytes) {
  let binary = '';
  for (let at = 0; at < bytes.length; at += 0x8000) binary += String.fromCharCode(...bytes.subarray(at, at + 0x8000));
  return btoa(binary);
}

/** `bitmap` at most MARK_REDUCED_SIDE pixels a side: a PNG, or a JPEG when the PNG is too large. */
async function reducedImage(bitmap) {
  const scale = Math.min(1, MARK_REDUCED_SIDE / Math.max(bitmap.width, bitmap.height));
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.round(bitmap.width * scale));
  canvas.height = Math.max(1, Math.round(bitmap.height * scale));
  canvas.getContext('2d')?.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  const encode = (/** @type {string} */ type) => new Promise(resolve => canvas.toBlob(resolve, type, 0.85));
  for (const type of ['image/png', 'image/jpeg']) {
    const blob = /** @type {Blob | null} */ (await encode(type));
    if (blob && blob.size <= MARK_IMAGE_BYTES) return new Uint8Array(await blob.arrayBuffer());
  }
  throw t('This image is too large even reduced; choose a simpler one.');
}

/**
 * Takes `file` as the band's own image, as the style will keep it: as it is when it fits;
 * otherwise a PNG or GIF goes to the host (`prepare`), which reduces it keeping its animation,
 * and a still JPEG is reduced here.
 * @param {File} file
 * @param {(data: string) => Promise<{ data: string }>} prepare
 */
async function chooseMarkImage(file, prepare) {
  const bytes = new Uint8Array(await file.arrayBuffer());
  const kind = sniffImage(bytes);
  const bitmap = kind ? await createImageBitmap(file).catch(() => null) : null;
  if (!bitmap) throw t('Choose a PNG, JPEG or GIF image.');
  try {
    const fits = bytes.length <= MARK_IMAGE_BYTES && Math.max(bitmap.width, bitmap.height) <= MARK_IMAGE_SIDE;
    if (kind === 'jpeg') {
      markImage = toBase64(fits ? bytes : await reducedImage(bitmap));
    } else if (fits && kind === 'png') {
      markImage = toBase64(bytes);
    } else {
      if (bytes.length > MARK_SOURCE_BYTES) throw t('The image is a file of up to 2.8 MB.');
      markImage = (await prepare(toBase64(bytes))).data;
    }
  } finally {
    bitmap.close();
  }
  // Unsigned, the image takes the emoji's place, so the band shows an icon.
  if (!$('#style-kuroko').checked) $('#style-icon').checked = true;
  renderStylePreview();
}

function removeMarkImage() {
  markImage = '';
  renderStylePreview();
}

/** The preview's band text; shown as a link, it looks like the link Teams shows. */
function previewLabel(text, linkLook) {
  const label = $('#style-preview-label');
  // A label with brackets of its own is not drawn as a link, though its band still opens the site.
  if (!linkLook || /[[\]]/.test(text)) {
    label.textContent = text;
    return;
  }
  const link = document.createElement('u');
  link.className = 'teams-link';
  link.textContent = text;
  label.replaceChildren(link);
}

/** The band height typed: 0 when empty (the band fits its text), null when the host would reject it. */
function bandHeight() {
  const typed = $('#style-band-height').value.trim();
  if (typed === '') return 0;
  const height = Number(typed);
  return Number.isInteger(height) && (height === 0 || (height >= 24 && height <= 160)) ? height : null;
}

/** The band padding typed: 0 when empty, null when the host would reject it. */
function bandPadding() {
  const typed = $('#style-band-padding').value.trim();
  if (typed === '') return 0;
  const padding = Number(typed);
  return Number.isInteger(padding) && padding >= 0 && padding <= 48 ? padding : null;
}

/** The icon size typed: 0 when empty (it follows the band text), null when the host would reject it. */
function markSize() {
  const typed = $('#style-mark-size').value.trim();
  if (typed === '') return 0;
  const size = Number(typed);
  return Number.isInteger(size) && (size === 0 || (size >= 16 && size <= 64)) ? size : null;
}

/** The emoji typed, as the host checks it: up to 16 characters, not ASCII but for keycaps, no spaces. */
function validEmoji(emoji) {
  const chars = [...emoji];
  return chars.length <= 16
    && chars.every(c => c.charCodeAt(0) > 127 || /[0-9#*]/.test(c))
    && (chars.length === 0 || chars.some(c => c.charCodeAt(0) > 127))
    && !/[\s\p{Cc}]/u.test(emoji);
}

/** The stripe width typed, when it is a whole number of pixels the host accepts. */
function stripeWidth() {
  const width = Number($('#style-stripe-width').value);
  return Number.isInteger(width) && width >= 2 && width <= 16 ? width : null;
}

function collectMessageStyle() {
  const stripe_width = stripeWidth();
  if (stripe_width === null) throw t('The stripe width is a whole number of pixels from 2 to 16.');
  const band_height = bandHeight();
  if (band_height === null) throw t('The band height is a whole number of pixels from 24 to 160, or empty to fit the text.');
  const band_padding = bandPadding();
  if (band_padding === null) throw t('The band padding is a whole number of pixels from 0 to 48, or empty for none.');
  const mark_size = markSize();
  if (mark_size === null) throw t('The icon size is a whole number of pixels from 16 to 64, or empty to follow the band text.');
  const emoji = $('#style-emoji').value.trim();
  if (!validEmoji(emoji)) throw t('The emoji is a single emoji, without words or spaces.');
  const hide_label = $('#style-hide-label').checked;
  if (hide_label && !$('#style-kuroko').checked && !$('#style-icon').checked) throw t('A band without text needs its icon: sign it as Kuroko or show an emoji or an image.');
  current.config.message_style = {
    // Retired on the host: every message is a card.
    enabled: true,
    label: $('#style-label').value.trim(),
    position: $('#style-position').value,
    accent: $('#style-accent').value.toUpperCase(),
    band: $('#style-band').value,
    label_color: $('#style-label-color').value,
    icon: $('#style-icon').checked,
    emoji: emoji || DEFAULT_EMOJI,
    band_height,
    stripe_width,
    text_size: $('#style-text-size').value,
    label_size: $('#style-label-size').value,
    kuroko: $('#style-kuroko').checked,
    animated: $('#style-animated').checked,
    mark_size,
    site_link: $('#style-site-link').checked,
    link_underline: $('#style-link-underline').checked,
    mark_side: $('#style-mark-side').value,
    band_align: $('#style-band-align').value,
    band_padding,
    hide_label,
    mark_image: markImage,
  };
}

export { renderChatList, STYLE_PRESETS, renderMessageStyle, renderStylePreview, collectMessageStyle, chooseMarkImage, removeMarkImage };
