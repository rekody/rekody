import { test, expect } from '@playwright/test';

const BASE = 'http://localhost:4321';

test.describe('SEO', () => {
  test('page title', async ({ page }) => {
    await page.goto(BASE);
    await expect(page).toHaveTitle(/rekody/i);
    await expect(page).toHaveTitle(/Voice Dictation for Everyone/i);
  });

  test('meta description contains key terms', async ({ page }) => {
    await page.goto(BASE);
    const desc = await page.locator('meta[name="description"]').getAttribute('content');
    expect(desc).toBeTruthy();
    expect(desc!.length).toBeGreaterThan(50);
    expect(desc!.length).toBeLessThanOrEqual(200);
    expect(desc!.toLowerCase()).toContain('voice dictation');
  });

  test('canonical URL', async ({ page }) => {
    await page.goto(BASE);
    const canonical = await page.locator('link[rel="canonical"]').getAttribute('href');
    expect(canonical).toBeTruthy();
    expect(canonical).toContain('rekody.com');
  });

  test('Open Graph tags', async ({ page }) => {
    await page.goto(BASE);
    const ogTitle    = await page.locator('meta[property="og:title"]').getAttribute('content');
    const ogDesc     = await page.locator('meta[property="og:description"]').getAttribute('content');
    const ogImage    = await page.locator('meta[property="og:image"]').getAttribute('content');
    const ogLocale   = await page.locator('meta[property="og:locale"]').getAttribute('content');
    const ogSiteName = await page.locator('meta[property="og:site_name"]').getAttribute('content');
    const ogImgAlt   = await page.locator('meta[property="og:image:alt"]').getAttribute('content');

    expect(ogTitle).toContain('rekody');
    expect(ogDesc).toBeTruthy();
    expect(ogImage).toContain('og-image.png');
    expect(ogLocale).toBe('en_US');
    expect(ogSiteName).toBe('rekody');
    expect(ogImgAlt).toBeTruthy();
  });

  test('OG image dimensions declared', async ({ page }) => {
    await page.goto(BASE);
    const w = await page.locator('meta[property="og:image:width"]').getAttribute('content');
    const h = await page.locator('meta[property="og:image:height"]').getAttribute('content');
    expect(w).toBe('1200');
    expect(h).toBe('630');
  });

  test('Twitter Card tags', async ({ page }) => {
    await page.goto(BASE);
    const card    = await page.locator('meta[name="twitter:card"]').getAttribute('content');
    const creator = await page.locator('meta[name="twitter:creator"]').getAttribute('content');
    const site    = await page.locator('meta[name="twitter:site"]').getAttribute('content');
    const imgAlt  = await page.locator('meta[name="twitter:image:alt"]').getAttribute('content');

    expect(card).toBe('summary_large_image');
    expect(creator).toContain('@');
    expect(site).toContain('@');
    expect(imgAlt).toBeTruthy();
  });

  test('robots meta tag', async ({ page }) => {
    await page.goto(BASE);
    const robots = await page.locator('meta[name="robots"]').getAttribute('content');
    expect(robots).toContain('index');
    expect(robots).toContain('follow');
  });

  test('robots.txt accessible', async ({ page }) => {
    const res = await page.goto(`${BASE}/robots.txt`);
    expect(res?.status()).toBe(200);
    const text = await page.content();
    expect(text).toContain('User-agent');
    expect(text).toContain('sitemap');
  });

  test('sitemap accessible', async ({ page }) => {
    const res = await page.goto(`${BASE}/sitemap-index.xml`);
    expect(res?.status()).toBe(200);
    const text = await page.content();
    expect(text).toContain('sitemap');
  });

  test('heading hierarchy — single h1', async ({ page }) => {
    await page.goto(BASE);
    const h1s = await page.locator('h1').all();
    expect(h1s.length).toBe(1);
    const h1Text = await h1s[0].innerText();
    expect(h1Text.length).toBeGreaterThan(0);
  });

  test('h2 section headings present', async ({ page }) => {
    await page.goto(BASE);
    const h2s = await page.locator('h2').all();
    expect(h2s.length).toBeGreaterThanOrEqual(4);
  });

  test('SoftwareApplication JSON-LD', async ({ page }) => {
    await page.goto(BASE);
    const scripts = await page.locator('script[type="application/ld+json"]').all();
    expect(scripts.length).toBeGreaterThanOrEqual(1);

    let found = false;
    for (const s of scripts) {
      const json = JSON.parse(await s.innerHTML());
      if (json['@type'] === 'SoftwareApplication') {
        expect(json.name).toBe('rekody');
        expect(json.operatingSystem).toBe('macOS');
        expect(json.offers?.price).toBe('0');
        expect(json.author?.name).toBeTruthy();
        found = true;
      }
    }
    expect(found).toBe(true);
  });

  test('FAQPage JSON-LD', async ({ page }) => {
    await page.goto(BASE);
    const scripts = await page.locator('script[type="application/ld+json"]').all();

    let found = false;
    for (const s of scripts) {
      const json = JSON.parse(await s.innerHTML());
      if (json['@type'] === 'FAQPage') {
        expect(json.mainEntity.length).toBeGreaterThan(0);
        expect(json.mainEntity[0]['@type']).toBe('Question');
        expect(json.mainEntity[0].acceptedAnswer['@type']).toBe('Answer');
        found = true;
      }
    }
    expect(found).toBe(true);
  });

  test('lang attribute on html element', async ({ page }) => {
    await page.goto(BASE);
    const lang = await page.locator('html').getAttribute('lang');
    expect(lang).toBe('en');
  });

  test('images have alt text', async ({ page }) => {
    await page.goto(BASE);
    const imgs = await page.locator('img').all();
    for (const img of imgs) {
      const alt = await img.getAttribute('alt');
      expect(alt, `img missing alt: ${await img.getAttribute('src')}`).toBeTruthy();
    }
  });

  test('favicon present', async ({ page }) => {
    const res = await page.goto(`${BASE}/favicon.ico`);
    expect(res?.status()).toBe(200);
  });
});
