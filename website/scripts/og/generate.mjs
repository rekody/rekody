// Render the OG/social card (template.html) to public/og.png at 1200×630.
// Run from website/: `node scripts/og/generate.mjs`
// Uses the playwright already in devDependencies (playwright.config.ts).
import { chromium } from 'playwright';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const template = path.join(here, 'template.html');
const out = path.join(here, '..', '..', 'public', 'og.png');

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1200, height: 630 }, deviceScaleFactor: 2 });
await page.goto('file://' + template, { waitUntil: 'networkidle' });
await page.evaluate(() => document.fonts.ready);
await page.screenshot({ path: out });
await browser.close();
console.log('wrote', out);
