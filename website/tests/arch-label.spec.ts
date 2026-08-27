import { test, expect, type Page, type BrowserContext } from '@playwright/test';

// The download button names the visitor's Mac without splitting the artifact:
// every state links to the same universal DMG, only the wording changes.
// These tests pin the two things that make that safe: the fail-safe ladder
// (a Mac we cannot identify keeps the neutral wording) and the reserved
// geometry (the wording changes without moving anything).

const BASE = 'http://localhost:4321';
const DMG = 'https://github.com/rekody/rekody-app/releases/latest/download/Rekody.dmg';

const CTA = '#hero a.pill-dark';
const TRUST = '#hero p.arch-swap';

// ── fake environments, installed before any page script runs ──────────────
const uaHints = (architecture: string) => `
  Object.defineProperty(navigator, 'userAgentData', { configurable: true, get: () => ({
    platform: 'macOS', mobile: false,
    getHighEntropyValues: () => Promise.resolve({ architecture: ${JSON.stringify(architecture)}, bitness: '64', platform: 'macOS' }),
  })});`;

const noUaHints = `Object.defineProperty(navigator, 'userAgentData', { configurable: true, get: () => undefined });`;

const gpu = (renderer: string, extensions: string[]) => `
  (() => {
    const real = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (type, ...rest) {
      const ctx = real.call(this, type, ...rest);
      if (!ctx || !String(type).startsWith('webgl')) return ctx;
      return new Proxy(ctx, { get(t, prop) {
        if (prop === 'getParameter') return (p) => (p === 0x9246 ? ${JSON.stringify(renderer)} : t.getParameter(p));
        if (prop === 'getSupportedExtensions') return () => ${JSON.stringify(extensions)};
        if (prop === 'getExtension') return (n) => (n === 'WEBGL_debug_renderer_info'
          ? { UNMASKED_RENDERER_WEBGL: 0x9246, UNMASKED_VENDOR_WEBGL: 0x9245 }
          : (${JSON.stringify(extensions)}.includes(n) ? {} : t.getExtension(n)));
        const v = t[prop];
        return typeof v === 'function' ? v.bind(t) : v;
      } });
    };
  })();`;

const noWebgl = `
  (() => {
    const real = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (type, ...rest) {
      return String(type).startsWith('webgl') ? null : real.call(this, type, ...rest);
    };
  })();`;

const APPLE_GPU_EXTS = ['WEBGL_debug_renderer_info', 'WEBGL_compressed_texture_pvrtc',
  'WEBGL_compressed_texture_astc', 'WEBGL_compressed_texture_etc'];
// PVRTC is deprecated. If a future Apple GPU drops it, ASTC plus ETC still answer.
const APPLE_GPU_EXTS_NO_PVRTC = ['WEBGL_debug_renderer_info',
  'WEBGL_compressed_texture_astc', 'WEBGL_compressed_texture_etc'];
const PC_GPU_EXTS = ['WEBGL_debug_renderer_info', 'WEBGL_compressed_texture_s3tc'];

const APPLE_M2_ANGLE = 'ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)';
const INTEL_ANGLE = 'ANGLE (Intel, Intel(R) Iris(TM) Plus Graphics OpenGL Engine, Unspecified Version)';

async function visit(context: BrowserContext, init: string | null): Promise<Page> {
  if (init) await context.addInitScript(init);
  const page = await context.newPage();
  await page.goto(BASE, { waitUntil: 'load' });
  return page;
}

/** Whatever the ladder decided, once it has had its idle slot to decide it. */
async function detected(page: Page): Promise<string> {
  await page.waitForTimeout(2500);
  return page.evaluate(() => document.documentElement.dataset.macArch ?? 'unset');
}

test.describe('architecture-aware download button', () => {
  test('server-rendered default is the neutral wording', async ({ page }) => {
    const html = await (await page.request.get(BASE)).text();
    expect(html).toContain('Download for Mac · free');
    expect(html).not.toContain('data-mac-arch');
  });

  test('Chromium on Apple silicon: UA Client Hints report arm', async ({ context }) => {
    const page = await visit(context, uaHints('arm'));
    expect(await detected(page)).toBe('apple-silicon');
    await expect(page.locator(CTA)).toHaveText(/Download for Apple Silicon · free/);
  });

  test('Chromium on an Intel Mac: x86 hints, confirmed by an Intel GPU', async ({ context }) => {
    const page = await visit(context, uaHints('x86') + gpu(INTEL_ANGLE, PC_GPU_EXTS));
    expect(await detected(page)).toBe('intel');
    await expect(page.locator(CTA)).toHaveText(/Download for Intel · free/);
    // The capability split is set before the download, not after.
    await expect(page.locator(TRUST)).toHaveText(/transcribes with Whisper/);
    await expect(page.locator(TRUST)).toHaveText(/real-time streaming needs Apple Silicon/);
  });

  test('an Apple GPU overrules an x86 hint', async ({ context }) => {
    const page = await visit(context, uaHints('x86') + gpu(APPLE_M2_ANGLE, APPLE_GPU_EXTS));
    expect(await detected(page)).toBe('apple-silicon');
  });

  test('Safari on Apple silicon: masked "Apple GPU" plus PVRTC', async ({ context }) => {
    const page = await visit(context, noUaHints + gpu('Apple GPU', APPLE_GPU_EXTS));
    expect(await detected(page)).toBe('apple-silicon');
    await expect(page.locator(CTA)).toHaveText(/Download for Apple Silicon · free/);
  });

  test('Safari on Apple silicon without PVRTC: ASTC plus ETC still answer', async ({ context }) => {
    const page = await visit(context, noUaHints + gpu('Apple GPU', APPLE_GPU_EXTS_NO_PVRTC));
    expect(await detected(page)).toBe('apple-silicon');
  });

  test('Firefox on an Intel Mac: the GPU names itself', async ({ context }) => {
    const page = await visit(context, noUaHints + gpu('AMD Radeon Pro 5500M OpenGL Engine', PC_GPU_EXTS));
    expect(await detected(page)).toBe('intel');
  });

  // ── everything below must land on the neutral wording ──────────────────
  test('masked GPU with no PVRTC stays neutral rather than guessing', async ({ context }) => {
    const page = await visit(context, noUaHints + gpu('Apple GPU', PC_GPU_EXTS));
    expect(await detected(page)).toBe('unset');
    await expect(page.locator(CTA)).toHaveText(/Download for Mac · free/);
  });

  test('a browser that farbles the renderer stays neutral', async ({ context }) => {
    // Brave 1.93+ reports the literal string "Brave" and randomises the
    // extension list, which names no GPU we can act on.
    const page = await visit(context, noUaHints + gpu('Brave', ['WEBGL_debug_renderer_info']));
    expect(await detected(page)).toBe('unset');
  });

  test('no WebGL stays neutral', async ({ context }) => {
    const page = await visit(context, noUaHints + noWebgl);
    expect(await detected(page)).toBe('unset');
  });

  test('an x86 hint alone is not enough to print "Intel"', async ({ context }) => {
    // x86 describes the browser build. Without a GPU to confirm the Mac, a
    // Rosetta browser on Apple silicon would be mislabelled, so stay neutral.
    const page = await visit(context, uaHints('x86') + noWebgl);
    expect(await detected(page)).toBe('unset');
    await expect(page.locator(CTA)).toHaveText(/Download for Mac · free/);
  });

  test('a non-Mac visitor stays neutral', async ({ context }) => {
    const page = await visit(context, `Object.defineProperty(navigator, 'userAgentData', { configurable: true, get: () => ({ platform: 'Windows', getHighEntropyValues: () => Promise.resolve({ architecture: 'x86' }) })});`);
    expect(await detected(page)).toBe('unset');
  });

  test('an iPad asking for the desktop site stays neutral', async ({ context }) => {
    const page = await visit(context, noUaHints + gpu('Apple GPU', APPLE_GPU_EXTS)
      + `Object.defineProperty(navigator, 'maxTouchPoints', { configurable: true, get: () => 5 });`);
    expect(await detected(page)).toBe('unset');
  });

  test('every state serves the same universal DMG', async ({ browser }) => {
    for (const init of [null, uaHints('arm'), uaHints('x86') + gpu(INTEL_ANGLE, PC_GPU_EXTS)]) {
      const context = await browser.newContext();
      const page = await visit(context, init);
      await detected(page);
      for (const href of await page.locator('a.pill-dark').evaluateAll((els) => els.map((e) => e.getAttribute('href')))) {
        expect(href).toBe(DMG);
      }
      await context.close();
    }
  });

  test('the wording changes without moving anything', async ({ browser }) => {
    const box = async (init: string | null) => {
      const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
      const page = await visit(context, init);
      await detected(page);
      const measured = await page.evaluate(([cta, trust]) => {
        const size = (sel: string) => {
          const r = document.querySelector(sel)!.getBoundingClientRect();
          return { w: Math.round(r.width), h: Math.round(r.height) };
        };
        return { cta: size(cta), trust: size(trust) };
      }, [CTA, TRUST]);
      await context.close();
      return measured;
    };

    const neutral = await box(null);
    const appleSilicon = await box(uaHints('arm'));
    const intel = await box(uaHints('x86') + gpu(INTEL_ANGLE, PC_GPU_EXTS));

    expect(appleSilicon).toEqual(neutral);
    expect(intel).toEqual(neutral);
  });

  test('the button still works with JavaScript disabled', async ({ browser }) => {
    const context = await browser.newContext({ javaScriptEnabled: false });
    const page = await context.newPage();
    await page.goto(BASE, { waitUntil: 'load' });
    const cta = page.locator(CTA);
    await expect(cta).toHaveText(/Download for Mac · free/);
    expect(await cta.getAttribute('href')).toBe(DMG);
    await context.close();
  });
});
