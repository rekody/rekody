# rekody.com Redesign Brief

**Date:** 2026-06-12 · **Status:** Draft for owner review · **Do not commit**

Synthesizes: full site audit (14 files), Wispr Flow / Screen Studio teardown, visual-tooling research, and owner direction (true copy, product-first structure, one-purpose CTAs, fix the language claim, kill the false pipeline, premium visuals, rethink the lockup).

---

## 1. Positioning & Voice

### Positioning statement

> **rekody is a consumer Mac dictation app with an open-source core.** You hold a key, speak, release — finished text lands at your cursor in any app. By default it runs entirely on your Mac: your audio never leaves the device, and the source code is public so you don't have to take our word for it. Pay once; the open-source CLI stays free forever.

The frame is **consumer app first, open source as the trust layer** — not "open-source CLI with an app coming." Wispr Flow's two structural gaps are rekody's identity: (1) local-by-default with zero caveats vs. their opt-in Privacy Mode with carve-outs, and (2) verifiable source vs. a closed binary. But copy their *form*: sell the outcome (speed, finished text, works everywhere) and make "never leaves your Mac" the trust clause beneath it — never lead with architecture.

One line that neither competitor can say, and that unifies pricing + privacy when the paid app ships:

> **"Pay once. Your voice never leaves your Mac."**

### Voice & tone rules

1. **Every sentence must be checkably true.** If a claim depends on which engine is active, say which engine. The Engines section's "Nemotron … English." note is the truth-telling template for the whole site.
2. **Outcome before mechanism.** "Finished text at your cursor" before "Rust binary." Mechanism appears only where it earns trust (privacy card, open-source page).
3. **Plain declarative sentences.** Subject, verb, object. Specific nouns and numbers ("5MB binary", "0 bytes sent") beat adjectives ("blazing", "seamless") every time.
4. **Ban the LLM tics found in the audit:**
   - The "No X, no Y, no Z" triad (currently used 3×: Demo:170, Features:165, OpenSource:199). Allow at most ONE instance site-wide — keep "No analytics. No tracking. No phone-home." in the privacy card, rewrite the others.
   - The staccato possessive triple ("Your voice. Your words. Your machine." — used twice). Cut both.
   - Two-beat fragment headlines ("One command. One minute." / "Six crates. One binary." / "MIT. Nothing fancy." / "Pick a path. Any works."). Five+ instances is the model's default register; keep at most two, write the rest as normal sentences.
   - Thesaurus swaps ("tongues" for "languages"), personification ("when the room demands it", "rekody doesn't care… it cares…"), "-grade" compounds ("desktop-grade latency"), strawman padding ("Faster than you expect."), abstraction filler ("no friction").
5. **One number per fact.** Latency currently appears as ~50ms, 194ms, and ~200ms. Measure the real first-token / ready figures, pick the defensible ones, anchor them ("first words on screen in ~200ms on an M-series Mac" style — Wispr anchors "4x faster" in wpm math; do the same), and use them identically everywhere including meta tags.
6. **The WordOrigin section is the voice benchmark.** Personal, specific, unhurried. Write toward that register.

### Before / after copy rewrites (examples of the register)

**A. Demo step 02 — language claim (Demo.astro:11)**
- Before: "language auto-detects across 100+ tongues"
- After: "Speak naturally. With the Whisper engine, rekody handles 100+ languages — the default on-device engine is English-only for now."

**B. Engines lede (Engines.astro:67)**
- Before: "Four speech-to-text backends. Cloud by default, fully local when the room demands it."
- After: "Five speech-to-text engines. On-device by default — cloud engines are there if you want them, and clearly labeled when you do."
  *(Pending the default-engine verification in §3. If the shipped default is actually cloud, the honest version is: "Five speech-to-text engines. Pick local-only and nothing ever leaves your Mac.")*

**C. Use-cases closer (UseCases.astro:254-257)**
- Before: "Your vocabulary. Your voice. Your cursor. rekody doesn't care what you do for a living. It cares that the word you just said is the word that lands on the screen."
- After: "Whatever you do for a living, the job is the same: the word you said shows up on the screen, spelled the way your field spells it."

---

## 2. Target Page Architecture

**CTA doctrine:** ONE primary action site-wide — **"Download for Mac" → `/#install`** (single destination; today the hero and navbar point at *different* places). ONE secondary — **"View source on GitHub" → repo**. GitHub is rekody's analog of Wispr's try-before-install web demo: for a privacy product, the repo *is* the demo. Nothing else gets a pill. Current state: ~27 CTA controls across 3 competing destinations; target: ≤10.

### Homepage (top to bottom)

| # | Region | Content | The ONE CTA |
|---|--------|---------|-------------|
| 1 | **Navbar** | Wordmark (new lockup, §4), 4 links | "Download" → `/#install`. **Drop the "Star" pill** (vanity ask; GitHub lives in footer + open-source page). |
| 2 | **Hero** | Keep "Speak. Release. Done." + deck + mock demo card (owner likes it). Fix card meta line (§3.1), align latency numbers, qualify "fully on-device". | "Download for Mac" → `/#install` (NOT the releases tarball). Keep the curl one-liner as the dev affordance — it's a copy button, not a competing CTA. |
| 3 | **See it work** *(new)* | The real Screen Studio recording: hold key → pill HUD → speech → text lands in a real app. Replaces the false "five stages" pipeline as the proof section. Keep the simple Hold/Speak/Release 3-step strip above it. | None. Click-to-play video only. |
| 4 | **Any app** | Existing logo wall + "If it accepts keystrokes, rekody writes into it." (owner favorite — untouched). | None. |
| 5 | **Languages** | Keep "Speak however you speak." headline; rebuild body with engine-qualified truth (§3.2). | None. |
| 6 | **Privacy** | Promote the report card ("Data sent to us: Nothing.") to its own full section — this is the positioning, not a sidebar. Add the verify-it-yourself proof point: "Don't take our word for it: read the source, or watch it with Little Snitch." Wispr buries privacy in a submenu *because their default is cloud + train-on-your-data*; rekody puts it above the fold of the scroll because it can. | "View source on GitHub" (the section's trust action — secondary style). |
| 7 | **Engines** | Keep the grid + honesty pattern. Fix count, default contradiction, Groq filler (§3). | None. |
| 8 | **Streaming / HUD preview** | Keep structure; fresh @2x assets (§4); "Coming soon" label stays adjacent to the HUD mock; fix daemon contradiction (§3.5). | None. |
| 9 | **Install** | Keep tabbed card. **Render the Download/View-source pills ONCE below the tabs, not inside all 5 panels** (currently 5× each in the DOM). Per-tab Copy buttons stay (functional, not conversion). Fix/cut the .dmg tab per §3.6. | "Download for Mac" (the section IS the destination; the pill here triggers the actual download once a .dmg truly ships — until then the tabs are the action). |
| 10 | **WordOrigin** | Keep as-is. Optional: trim "A record is what a voice leaves behind…" if it reads precious on re-read. | None (Pronounce button is functional). |
| 11 | **Ask-an-AI block** *(new, optional, cheap)* | Pre-filled "Ask ChatGPT / Claude / Perplexity about rekody" prompt buttons — Wispr's LLM-SEO play, directly stealable. | The prompt buttons (utility, not conversion). |
| 12 | **Closing CTA** | One line + one pill. | "Download for Mac" → `/#install`. |
| 13 | **Footer** | 3 link columns survive; fix brand blurb (§3.2). | None (links are navigation, not CTAs). |

### /use-cases
Keep the 9 profession cards + accessibility callout verbatim (strongest copy on the site). Rewrite the multilingual block per §3.2. Verify-or-cut the `/sig` `/pr` dictionary chips and "Works with VoiceOver" (§3.7). Closing region: ONE pill — "Download for Mac" → `/#install`; cut the second "See install options" ghost pill (same destination twice).

### /open-source
Keep crates grid + MIT section + contribute cards. Collapse the **three** GitHub CTAs (hero pill, footer pill, plus per-crate links) to: hero "View source on GitHub" pill + per-crate links (those are navigation). Cut the footer dark pill. Fix "free users" and "11 providers" (§3.8, §3.9). The install tabs here lose their duplicated pills the same way as the homepage.

### Later: /pricing (when the paid app is announced)
Classic Screen Studio presentation, in plain sentences on the card: **pay once · N Macs · 1 year of updates · your version works forever · optional update renewal**. Time-boxed full-featured trial, no credit card (NOT a word-count-gated free tier — wrong mechanic for one-time licensing). FAQ pre-answers: "What happens after my year of updates?", "How many Macs?", "Refunds?". Price sustainably from day one — Screen Studio underpriced at $229 and rug-pulled to subscriptions in Sept 2025; never be in that position. Until this page exists, the site makes **zero** paid-tier references (see §3.8).

---

## 3. Claims Policy — exact fixes

**The site-wide language pattern** (already correct in one place, OpenSource.astro:215): every language claim is qualified **per-engine**. Canonical wording: **"100+ languages with the Whisper engine. The default on-device streaming engine is English-only (for now)."** Short form where space is tight: **"100+ languages (engine-dependent)"**.

1. **Hero mock card (Hero.astro:126-128):** "Nemotron · streaming" + "auto · 100+ langs" is FALSE — Nemotron streaming is English-only. Change meta line to `Nemotron · streaming · on-device · en`. (Bonus: this then matches the cli screenshot badge "nemotron · en".)
2. **Unqualified "100+ languages" — replace at every instance:**
   - Demo.astro:11 → rewrite per §1 example A.
   - Features.astro:116 eyebrow → "100+ languages with Whisper".
   - Features.astro:121 + 151 → delete the Nova-3 credits entirely ("Nova-3 hears the language… switches mid-sentence", "…and ~80 more. Every language Deepgram Nova-3 supports"). Nova-3 does not support 100+ languages; the 100+ figure is Whisper's. Rebuilt section: keep headline, then "With the Whisper engine, rekody transcribes 100+ languages — pick your language or let it detect. The default on-device streaming engine is English-only today." Keep the language-name parallax columns (they're pretty and now honestly attributed).
   - Footer.astro:52 → "…in any app, in 100+ languages (engine-dependent)" or simply "…in any app."
   - UseCases.astro:227 → "Every profession above works in 100+ languages when you use the Whisper engine. The default streaming engine is English-only for now." Delete "Auto-detect is on by default."
   - Base.astro:14 meta → keep the existing "via Whisper" qualifier, align latency number with the hero's verified figure.
3. **"Five stages, start to cursor" (Demo.astro:21-27, 63):** DELETE the conveyor animation and copy. Owner confirms it isn't true, and it hardcodes Deepgram Nova-3 + smart_format as THE architecture under an on-device hero. Replacement is the real recording (§2 region 3). Do not rebuild a "stages" metaphor.
4. **Default-engine contradiction — pick ONE truth.** Engines.astro:5 says Nemotron is "Default"; Engines.astro:67 says "Cloud by default"; privacy.astro:38 says "the default local Whisper engine". **Action: verify the shipped default in the CLI source/config first** (per repo verification rule — don't infer). Then state it identically in all three places. If the default is Nemotron on-device streaming (which the hero's "fully on-device" implies), the privacy page line becomes "the default on-device engine" and the Engines lede follows §1 example B.
5. **Daemon contradiction:** Demo.astro:170 "no background daemon" vs Streaming.astro:27 "The open-source daemon already speaks to it." Verify the architecture; keep whichever is true, rewrite the other. (If a daemon exists, Demo's line becomes "No browser, no Electron — one Rust binary," which also clears one banned triad.)
6. **InstallSection.astro:45 ".dmg tab":** repo CLAUDE.md says releases ship `.tar.gz` + SHA256SUMS. If no signed .dmg ships today, DELETE the tab and the "no terminal required" note. Reinstate when the paid app actually ships a .dmg. Never let "Download for Mac" land a non-terminal user on a CLI tarball without saying so.
7. **UseCases.astro:193 chips:** verify "/sig", "/pr" snippet/dictionary features and "Works with VoiceOver" are shipped in the current CLI. Keep what's shipped; move the rest to a clearly-labeled "coming in the Mac app" treatment or cut.
8. **OpenSource.astro:18 "Whisper tiny runs locally for free users":** implies an unannounced paid tier. Rewrite: "Whisper tiny runs locally out of the box." Zero paid-tier references site-wide until /pricing exists.
9. **OpenSource.astro:23 "11 providers" vs 8 logos on Engines.astro:** count the providers in `rekody-llm`'s source, use that number in both places.
10. **Engines.astro:67 "Four speech-to-text backends":** five cards. Say "Five."
11. **Latency:** one verified ready-time, one verified first-token/total figure, used identically in Hero deck, hero card, install copy, and Base.astro meta. Delete numbers that can't be reproduced.
12. **Engines.astro:24 Groq card:** replace "Faster than you expect." with a fact (e.g., the model + a measured characteristic) or just end at "Whisper large-v3, hosted."
13. **"Fully on-device" (Hero.astro:31):** true for 2 of 5 engines. Qualify: "On-device by default" (if §3.4 verification holds) — accurate and stronger anyway.
14. **HUD teaser (Streaming.astro:25-30):** keep, but present tense → future ("will show the same live transcription"), "Coming soon" label stays visually attached to the mock.
15. **Base.astro:47-48:** og:image meta says 1200×630, file is 2400×1260 — fix the meta (or export a 1200×630).

---

## 4. Visual Production Plan

Principle (from the Linear/Raycast research): **product UI is rendered as real HTML/CSS and animated in code — never exported as images.** A terminal and a macOS pill are the two easiest surfaces to fake pixel-perfectly in DOM: retina-sharp at every DPI, dark-native, free to iterate, agent-editable forever. AI images are for atmosphere only; one real recording is the proof.

**Production order** (each step unblocks the next):

1. **Logo + wordmark lockup rethink** — *Paper (canvas exploration) → SVG.* Logo mark survives (owner likes it); the lockup is the problem. Explore: mark-only at small sizes, lowercase wordmark with tightened tracking, vertical spacing variants, monochrome + accent versions. Deliver as inline SVG for Navbar/Footer + favicon/touch-icon exports (fixes the 41KB 1024px apple-touch-icon while in there — export a real 180px).
2. **HTML/CSS product mockups** — *agent-built Astro components, no tool.* (a) Terminal frame component (replaces cli-streaming.png — kills the v0.5.9 staleness, the soft 1x rendering, and the dead bottom 40% in one move; the streamed text can read "nemotron · en" honestly). (b) Pill HUD component (replaces hud-pill.png — fixes the clipped mid-phrase text; "Coming soon" badge built in). These double as animation targets.
3. **Hero/section motion** — *Motion (motion.dev), vanilla JS via `<script>` tags (~2.3–18kb), free/MIT.* The hero sequence: pill HUD springs in → waveform bars pulse → text streams character-by-character into the DOM terminal, cursor blinking, long-pause loop. React island (`client:visible`) only if the orchestration demands it. `prefers-reduced-motion` respected everywhere. GSAP (now free) as fallback for any scroll-scrubbed sequence.
4. **The real recording** — *Screen Studio ($29 for one month covers all launch assets). THE ONLY MANUAL STEP — Tony records (~15-30s): hold key, speak, release, text lands in a real app.* Agent handles everything after export: ffmpeg → 1080p H.264 MP4 + WebM, poster frame extracted (so LCP isn't the video), embedded `<video muted playsinline preload="metadata" poster>` click-to-play in region 3.
5. **Atmosphere assets** — *HF MCP (FLUX.1-Krea-dev / Z-Image-Turbo / Qwen-Image via the existing illustrations pipeline), free on ZeroGPU.* Dark gradient/ink textures, grain backdrops, OG-card background. **Never for UI/text** — one wrong glyph kills the premium feel. Always post-processed: grade to site palette + grain overlay. Background texture in-page is pure CSS (layered radial-gradients + SVG feTurbulence data-URI) — zero network cost.
6. **OG image rebuild** — composite: atmosphere background (step 5) + new lockup (step 1) + one true tagline. Export 1200×630 to match the fixed meta.
7. **Image hygiene** — all remaining `<img>` get width/height attributes (Streaming.astro currently shifts layout); below-fold images through `astro:assets` for AVIF/WebP + srcset.

**Evaluated and rejected for v1:** Rive (highest ceiling, but authoring needs manual editor skill — agent can't carry it), Jitter/Lottie (everything it exports, Motion does in code with smaller payloads), Canva (template-grade ceiling, wrong aesthetic). Remotion (skill already installed) is the fallback if a video asset is needed and Tony can't record.

---

## 5. Build Plan — ordered, agent-sized steps

Survival map first. **Survives as-is:** WordOrigin.astro, LegalPage.astro, Lettermark.astro (pending step 9), use-case cards + accessibility callout, any-app logo wall, privacy report card, crates grid, MIT section, tabbed-install UI mechanism, page wrappers. **Survives with edits:** Hero, Features, Engines, Streaming, InstallSection, Navbar, Footer, Base, privacy.astro, OpenSource, UseCases. **Dies:** Demo.astro's five-stage pipeline animation, cli-streaming.png, hud-pill.png, the .dmg tab (until a .dmg ships), the Star pill, ~17 duplicate CTAs.

Each step is one agent session, independently shippable, ordered so the site is never *more* false than the day before:

1. **Truth pass (copy-only, no layout).** Apply every §3 fix across the 11 files with edits. Prerequisite verifications in the CLI repo first: shipped default engine, daemon yes/no, .dmg yes/no, dictionary//sig//pr/VoiceOver shipped, LLM provider count. Biggest honesty win, zero design risk. *(Components touched: Hero, Demo, Features, Engines, Streaming, InstallSection, UseCases, OpenSource, Footer, Base, privacy.)*
2. **De-LLM voice pass (copy-only).** Apply §1 rules: triads down to one, possessive triples cut, fragment headlines down to two, all flagged phrases rewritten. Read everything aloud once.
3. **CTA consolidation.** Navbar: drop Star, Download → `/#install`. Hero pill → `/#install`. InstallSection: pills rendered once below tabs. UseCases closer: one pill. OpenSource: one GitHub pill. Footer blurb fix rides along. ~27 → ≤10 controls, 2 destinations.
4. **Kill the pipeline, build region 3 shell.** Delete the five-stage conveyor from Demo.astro; keep Hold/Speak/Release strip; stub the "See it work" section with a poster placeholder so step 8 drops the video in.
5. **HTML/CSS terminal + pill HUD components** (visual plan step 2). Replace both PNGs in Streaming.astro; delete the stale assets; add width/height everywhere.
6. **Hero motion** (visual plan step 3). Motion via script tags; reduced-motion fallback is the static composed state.
7. **Atmosphere + OG** (visual plan steps 5-6). CSS texture layer, graded AI backdrops where sections need depth, new og.png, fix og meta, 180px touch icon.
8. **Drop in the real recording** (visual plan step 4 — blocked on Tony's one recording session; everything else proceeds without it).
9. **Lockup integration** (visual plan step 1 output) into Navbar/Footer/Lettermark/favicon/OG.
10. **QA pass.** `/rams` accessibility + visual review, reduced-motion check, Lighthouse/CLS (the width/height + poster work should show up here), every claim re-read against §3, all CTAs clicked, meta validated.
11. **Later, gated on owner go:** /pricing per §2, "Ask an AI" block, .dmg tab reinstated when the Mac app ships.

**Definition of done for copy:** a skeptical HN commenter can check any sentence on the site against the repo and the shipped binary, and lose.
