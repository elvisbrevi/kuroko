// Renders the app icon (desktop/icons/source/app.png: Kuroko, the clay kuroko's head on a full-bleed
// cream square, kept outside the crate) to the images the GUI (local page and portal), the PWA, the
// landing and the Teams band use. iOS gets the full square, which its own rounded mask cuts; browsers,
// the PWA manifest and the band get a rounded tile with transparent corners.
// Usage: node scripts/render-icons.mjs   (Playwright from scripts/ui-preview/deps.mjs)
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { launch } from './ui-preview/deps.mjs';

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = `data:image/png;base64,${fs.readFileSync(path.join(root, 'desktop/icons/source/app.png')).toString('base64')}`;
// The tile's corner radius, as a share of its side (close to the iOS mask).
const RADIUS = 0.225;
const outputs = [
  { name: 'desktop/ui/apple-touch-icon.png', size: 180, opaque: true },
  { name: 'desktop/ui/icon-192.png', size: 192 },
  { name: 'desktop/ui/icon-512.png', size: 512 },
  { name: 'desktop/ui/favicon.png', size: 64 },
  // The mark before the Teams band's label (src/adapters/card.rs embeds it in every card): reduced
  // to a palette with ImageMagick, about 1 KB.
  { name: 'src/adapters/kuroko-badge.png', size: 40, colors: 64 },
  // The landing's brand mark, tab icon and home-screen icon.
  { name: 'site/public/assets/icon.png', size: 256 },
  { name: 'site/public/assets/apple-touch-icon.png', size: 180, opaque: true },
];
const browser = await launch();
const page = await browser.newPage();
async function render(output) {
  const data = await page.evaluate(async ({ source, output, radius }) => {
    const image = new Image();
    image.src = source;
    await image.decode();
    const canvas = document.createElement('canvas');
    canvas.width = canvas.height = output.size;
    const g = canvas.getContext('2d');
    g.imageSmoothingQuality = 'high';
    if (!output.opaque) {
      g.beginPath();
      g.roundRect(0, 0, output.size, output.size, output.size * radius);
      g.clip();
    }
    g.drawImage(image, 0, 0, output.size, output.size);
    return canvas.toDataURL('image/png');
  }, { source, output, radius: RADIUS });
  return Buffer.from(data.split(',')[1], 'base64');
}
for (const output of outputs) {
  const file = path.join(root, output.name);
  fs.writeFileSync(file, await render(output));
  if (output.colors) execFileSync('convert', [file, '-strip', '-colors', String(output.colors), file]);
  console.log(`${output.name} (${output.size} × ${output.size})`);
}
await browser.close();
