# rekody Paid Launch — Master Spec

**Status:** Living doc · **Created:** 2026-04-21 · **Last updated:** 2026-04-22 · **Target:** Paid v1 with 14-day no-card trial · **Earliest responsible launch:** 2026-09-29

This document synthesizes five parallel deep dives (codebase audit, licensing architecture, tier design, ops/signing, launch roadmap) into one executable plan. Where the five disagreed, open decisions are called out in §14. Strategic decisions made after initial drafting are captured in §1.5 (public-comms posture), §1.6 (repo topology), and updated §10.2 (legal surface status).

---

## 1. Executive Summary

rekody today is a **clean, MIT-licensed Rust CLI** with strong foundations (Keychain for secrets, 11 LLM providers, 4 STT providers, local Whisper fallback, modular 6-crate workspace). It has **zero infrastructure for commercialization** — no licensing, no signing, no notarization, no update verification, no telemetry, no pricing page, no Tauri GUI. Every mockup in Paper (Dictate, History, Dictionary, Snippets, STT Engine, Polish Engine, Hotkey, Preferences, Onboarding, HUD) implies a webview that does not exist in the repo.

The paid product is a **hosted service** — managed Deepgram + Polish proxy, cloud sync of dictionary/snippets/profiles, and the convenience layer that removes API-key management. The OSS binary stays MIT and fully functional with user-supplied keys; Pro is the managed experience for people who don't want to manage plumbing.

**Launch blockers, ranked by severity:**

1. No code signing or notarization — Gatekeeper will quarantine every install.
2. No Tauri v2 shell — every designed page assumes a webview that hasn't been scaffolded.
3. No licensing subsystem — must build entitlement server + JWT refresh + Keychain storage from zero.
4. No API-key proxy — the single largest UX delta vs. BYOK must run server-side.
5. Release artifacts still named `chamgei-*` — will break existing Homebrew users on first rename.

Everything below is gap-list work, not retrofit.

---

## 1.5 Public-Facing Communication Posture

Added 2026-04-22 after a public-comms audit surfaced that the shipped privacy/security/terms pages had drifted into publishing our build plan and evangelizing the feature that cannibalizes Pro.

Three rules govern anything a customer can see — website copy, legal pages, docs, README, launch posts (PH / HN / X), email templates, support macros, in-app explanatory copy.

**1. We do not evangelize BYO-key mode.**
BYO is a supported configuration in Pro for users with strict data-path requirements. It stays a feature. It is not a marketing message. Concretely:
- No "BYO" headline, hero bullet, or feature card on the marketing site.
- Not listed as a Pro differentiator on `/pricing`.
- Legal pages disclose it where required (ToS carve-out of user responsibility for third-party providers) without promotional framing — no "cut us out of the data path" language.
- In-app: surfaced as a configuration option in Settings, not as a top-level mode toggle.

Rationale: the public pitch for Pro is managed infrastructure. Evangelizing BYO in marketing trains prospects to use the free escape hatch, which defeats the revenue plan that justifies building Pro at all.

**2. We only publish live sub-processors.**
The `/subprocessors` page lists vendors actually processing data today. Planned vendors do not appear. A planned-vendor table telegraphs our entire build plan — what billing provider we'll use, what auth, what proxy stack, what STT/LLM vendors we're considering. None of that is legally required for pre-launch software; disclosure obligation attaches to actual processing, not roadmap.

New vendors get added to `/subprocessors` when they go live, with 30-day advance email to active subscribers for material changes.

**3. Architectural implementation details stay internal.**
Trust signals are public; plumbing is internal.

| Public copy says | Internal docs say |
|---|---|
| "cryptographically signed tokens, re-validated regularly, with offline tolerance" | Ed25519-signed JWT, 24h refresh, 7-day offline grace |
| "streamed, not persisted — audio held in memory only" | 4-step proxy lifecycle with buffer-lifetime semantics |
| "speech-to-text and LLM providers" in narrative text | Deepgram Nova-3, Groq, Anthropic Haiku, OpenAI 4o-mini (named only on `/subprocessors` once live) |
| "zero-retention agreements where available" | Specific ZDR program names per vendor |
| "hardware-backed key management" | KMS vendor + rotation cadence |

**What stays fully public:** zero-retention commitment, no-training commitment, no-sale commitment, AES-256 at rest, TLS 1.3 in transit, signed + notarized + hardened-runtime binaries, the MIT-licensed engine, standard data rights (GDPR/CCPA).

**Enforcement.** Every customer-facing page gets a pre-publish pass against §1.5 before merge. If a passage names a specific algorithm, refresh interval, vendor, or architectural step count, it belongs in internal docs, not on the site.

---

## 1.6 Repo Topology

Two-repo split, agreed earlier in the paid-launch planning process:

- **`rekody` (public, MIT)** — current repo at `/Users/tonykipkemboi/Startups/chamgei` (directory legacy-named; all code is `rekody`). Contents: 6-crate dictation engine (`rekody-core` / `-audio` / `-stt` / `-llm` / `-inject` / `-hotkey`), `install.sh`, Homebrew tap, Astro website with `/open-source` + legal surface. Stays MIT forever.
- **`rekody-pro` (private)** — new repo to be created. Contents: Tauri v2 desktop app (`apps/desktop/*`), services (`services/api`, `services/proxy`), paid-only crates (`rekody-ipc`, `rekody-license`, `rekody-telemetry`), proprietary UI, proprietary licensing logic.

**Cross-repo dependency strategy:** publish the 6 OSS crates to crates.io from the public repo on every tagged release. The private repo depends on them by version (`rekody-core = "0.6"`) rather than path, so the public repo is the one source of truth for the engine.

Implications to update elsewhere in this spec:
- §3 / §12.1 layout diagrams still show a single monorepo — update alongside Phase 0 scaffolding work.
- §9.6 distribution: two signing identities? TBD — Apple Developer ID is per-team, single identity probably fine.
- §13.1 Phase 0: add "Publish OSS crates to crates.io; tag v0.6" as a precursor to the `rekody-pro` repo standup.

---

## 2. Current-State Audit

### 2.1 Architecture

Workspace `/Users/tonykipkemboi/Startups/chamgei/Cargo.toml` v0.5.1, 6 member crates:

- `rekody-core` — CLI entrypoint, history, dictionary, snippets, corrections, prompts, status, update
- `rekody-audio` — cpal capture + rubato resampling + RMS-based VAD
- `rekody-stt` — Deepgram, Groq, local Whisper (whisper-rs), Cohere local server
- `rekody-llm` — 11 providers (Groq, Cerebras, Together, OpenRouter, Fireworks, OpenAI, Anthropic, Gemini, Ollama, LM-Studio, vLLM, custom) with first-success failover
- `rekody-inject` — clipboard + Cmd+V, or native CGEvent
- `rekody-hotkey` — macOS CGEventTap, ⌥Space default, push-to-talk or toggle

**Not Tauri-based.** `grep tauri` across `**/Cargo.toml` returns empty. The binary `/usr/local/bin/rekody` is the entire product. Every design artboard (Dictate, History, Dictionary, Snippets, STT Engine, Polish Engine, Hotkey, Preferences, Onboarding, HUD) is unimplemented.

### 2.2 Data & secrets

| Item | Location | Format | Notes |
|---|---|---|---|
| Config | `~/.config/rekody/config.toml` | TOML | `chmod 0600`, user-edited |
| History | `~/.config/rekody/history.json` | JSON array | max 5,000 entries, plain text |
| Dictionary | `~/.config/rekody/dictionary.toml` | TOML | custom vocabulary |
| Snippets | `~/.config/rekody/snippets.toml` | TOML | trigger → expansion |
| Audio | (ephemeral, in-memory) | — | not persisted |
| API keys | macOS Keychain | `keyring` v3, service `com.rekody.voice` | `rekody key set/list/delete` |

No encryption at rest for history/config/dictionary/snippets. Plans below do not change that for v1 — adding encryption without key escrow creates a worse bug class (user locked out of own data) than it solves.

### 2.3 Distribution today

- GitHub Releases: `.tar.gz` for aarch64 + x86_64, **unsigned**, SHA256SUMS generated but **never verified by the client**.
- Homebrew tap: `rekody/homebrew-rekody` (formula, not cask — CLI only).
- Install script: `install.sh` via `curl | bash`.
- CI: `.github/workflows/release.yml` builds on `macos-14`, no signing/notarization jobs.
- **Legacy filename bug**: `dist/chamgei-v0.4.2-macos-arm64.tar.gz` — rename before v1 or existing Homebrew formulas break.

### 2.4 Website

Astro 5 at `/website/`, deployed to Vercel, site `https://rekody.com`. Version flows from root `Cargo.toml` → `website/src/lib/version.ts` at build. **No pricing page.** Hero, Features, Demo, Engines, InstallSection exist.

### 2.5 Security posture (good so far)

- API keys in Keychain, never in config.toml.
- No hardcoded secrets, no telemetry, no phone-home.
- `SECURITY.md` published with `security@rekody.com` disclosure path. **DNS routing unverified** — fix before any paid marketing.
- No GPL/AGPL/SSPL deps in Cargo.lock.

### 2.6 Known gaps flagged in code

- `crates/rekody-core/src/context.rs:103` — Windows context detection TODO.
- `crates/rekody-llm/src/lib.rs:143` — local LLM inference stub.
- Update mechanism downloads over HTTPS but doesn't validate signatures.

---

## 3. Target Architecture

```
                  ┌──────────────────────────────────────┐
                  │         rekody desktop (macOS)       │
                  │  ┌────────────────────────────────┐  │
                  │  │  Tauri v2 shell (new)          │  │
                  │  │  React 19 + Vite + Tailwind 4  │  │
                  │  └────────────┬───────────────────┘  │
                  │               │ IPC (tauri-specta)   │
                  │  ┌────────────┴───────────────────┐  │
                  │  │  rekody-core + audio/stt/llm/  │  │
                  │  │  inject/hotkey/ipc/license/    │  │
                  │  │  telemetry  (Rust workspace)   │  │
                  │  └────────────┬───────────────────┘  │
                  └───────────────┼──────────────────────┘
                                  │
                ┌─────────────────┼─────────────────┐
                │                 │                 │
         (BYO keys path)   (Pro managed path)  (update check)
                │                 │                 │
                ▼                 ▼                 ▼
        Deepgram / Groq /    api.rekody.com    updates.rekody.com
        OpenAI / Anthropic   (Hono on Vercel,  (CF Worker + R2)
        / local Whisper      Neon Postgres,
                             Stripe webhooks)
                                  │
                                  ▼
                           Proxy service
                           (Rust/Axum on Fly.io
                           OR Hono edge — see §14)
                                  │
                                  ▼
                          Deepgram / Groq /
                          Claude Haiku / GPT-4o-mini
                          (server-held keys, per-user metering)
```

**Separation of concerns:**

- **Client (Rust + Tauri):** owns audio capture, hotkeys, injection, local storage, Keychain, license verification. Never holds the proxy provider keys.
- **API (`api.rekody.com`):** auth (magic link), license issuance, Stripe webhook, entitlement JWT, device management.
- **Proxy (`proxy.rekody.com` or same API host):** validates JWT, forwards streaming STT / Polish, meters usage, enforces per-user budget caps.
- **Updates (`updates.rekody.com`):** CF Worker returns signed manifest; R2 hosts DMGs. Worker can gate paid-channel builds by JWT.
- **Website (`rekody.com`):** Astro on Vercel — marketing, pricing, legal, docs, changelog.

---

## 4. Tier Definitions

### 4.1 Free (stays MIT, forever)

A complete, honest dictation app. Not a demo.

- Unlimited dictation with user-supplied keys (Deepgram / Whisper API / Groq / Fireworks / local whisper.cpp)
- Local-only STT via whisper.cpp small/medium (zero cost, zero network)
- Polish step with user-supplied LLM key, **1 preset** ("Clean up")
- 1 global hotkey, all input modes (push-to-talk, toggle, hands-free VAD)
- History: **last 100 transcripts**, searchable, local-only
- Dictionary: **50 custom terms**
- Snippets: **10 static text expansions**
- App-aware paste, 1 STT profile + 1 Polish profile
- Export as .txt / .md / .json
- All settings pages fully functional — no "upgrade to configure" walls

### 4.2 Pro ($9/mo, 14-day trial, no card)

Managed infrastructure + power features.

**Public pitch:** managed, hands-off, no keys to manage, Dictionary/Snippets/Profiles synced across every Mac. This is the entirety of how Pro gets described on the website and in launch copy — see §1.5.

**BYO-key configuration exists but is not marketed.** Pro retains the ability to route a specific engine through a user-supplied API key, surfaced as a configuration option in Settings (not a top-level mode toggle). It's there for users with strict data-path requirements and for BYO-curious OSS users who convert to Pro and want to keep an existing provider account. It is **not** a Pro differentiator in headlines, hero copy, pricing cards, feature comparisons, or legal-page callouts.

**Managed:**
- Managed Deepgram Nova-3 streaming (no keys, no billing, no setup)
- Managed Polish via Claude Haiku / GPT-4o-mini
- End-to-end encrypted sync (Dictionary, Snippets, Profiles, Hotkeys) across every Mac

**Power features:**
- Unlimited STT + Polish profiles
- Context-aware profile auto-switching (bundle ID + window title → "Clinical / Slack / Xcode / Email" profiles)
- Unlimited dictionary, unlimited snippets with dynamic variables (`{{date}}`, `{{clipboard}}`, `{{selection}}`)
- Multiple hotkeys, one per profile
- History: unlimited, full-text search, tag + pin, 1-year retention
- Polish rewind (diff view, revert to raw)
- Custom polish prompts, shared across devices
- Streaming partial output into cursor (Wispr-style)
- In-stream voice commands ("new paragraph," "scratch that")
- Speaker-aware mode (interviews)
- Batch file transcribe
- Priority model access
- Analytics dashboard (words/day, WPM, time saved)
- Export +.srt / .vtt / .docx

### 4.3 Teams — explicitly v1.1

Defer until ≥30 paying Pro users ask. v1.1 sketch: $15/seat/mo, 3-seat minimum, shared dictionaries, shared snippet libraries, admin-managed fair-use pool, SAML SSO, audit log, BAA for clinical orgs.

### 4.4 Fair-use metering

**Cost math (Deepgram Nova-3 late-2025: $0.0043/min + Haiku Polish ≈ $0.0028/min = ~$0.0071/min all-in):**

| Cap | Value | Rationale |
|---|---|---|
| Soft cap (warn) | 800 minutes / mo | 80% of hard cap, non-modal banner |
| **Hard cap** | **1,000 managed minutes / mo** | ~150k words ≈ 45 min/business day. 40% margin on $9. |
| Polish calls | 3,000 / mo | Tracked separately, rarely binds |
| Over-cap | $5 one-tap top-up for +500 min | No auto-enroll, no nag |

**Graceful fallback, not hard lock.** At 100% the proxy returns a specific error code; client auto-falls-back to user's BYO key if configured, otherwise to local whisper.cpp. Transcript is tagged "transcribed locally." Dictation never stops. This is non-negotiable UX — hard-locking a paying user mid-sentence kills retention.

**Server-side kill switch.** Admin endpoint can force-cap any account instantly without shipping an app update (for abuse response). Redis `user:{id}:usage_cents_mtd` counter with `INCRBYFLOAT` per request.

---

## 5. Trial Mechanics

### 5.1 Trial shape

- **14 days, no card required** at start.
- Trial begins on magic-link sign-in, not on download.
- Trial state lives server-side; `trial_ends_at = now() + 14d`.
- **Soft device hash** attached for abuse monitoring (not blocking):

```
device_hash = SHA256(
    IOPlatformUUID_of_home_volume
  + macOS_user_shortname
  + per_install_salt
)
```

Not hardware serial. Not MAC. A legit disk wipe regenerates the salt → fresh hash. `≥2` prior trials on the same hash → flag for manual review, **do not auto-block**.

### 5.2 Trial → Pro conversion UX

- **Day 11:** cream banner at top of Dictate — *"3 days left on your Pro trial. You've dictated 47,000 words with managed Deepgram so far."* Buttons: *Continue Pro — $9/mo* · *Remind me later*. No modal.
- **Day 13:** in-app + opt-in email, same style, updated word count.
- **Day 14 (final day, first launch of the day):** single sheet, once. Fraunces title: *"Your trial ends tomorrow."* Source-Serif italic body: *"Tomorrow rekody returns to the free tier. Your Dictionary, Snippets, and History stay — you won't lose anything."* One CTA: *Keep Pro — $9/mo*. One quiet link: *Let it expire*.
- **Day 15:** silent downgrade. Non-modal bar for 72h: *"You're on Free. Your data is intact."* One link: *Restart Pro*. Then bar disappears. No further prompts.

**What we do not do:** require a card to start, send day-1/3/5/7 nags, show a countdown timer, hide the "let it expire" link, weight buttons toward upgrade, or send marketing email without explicit opt-in at Pro signup.

### 5.3 Three in-app upgrade moments (Free tier)

1. **Second profile attempt.** Sheet explains profiles + register-switching; two equally-weighted buttons (*Start trial* / *Keep one profile*). Not greyed out.
2. **101st history item.** Soft cream divider: *"Earlier transcripts are kept locally on Pro. Your first 100 are always here."* No popup.
3. **Key-rotation moment.** When a BYO key expires or rate-limits mid-dictation: *"Your API key stopped responding. Still transcribing locally. Pro handles key management for you — [Try it]."*

**Rule:** each surface re-prompts at most once per 7 days.

---

## 6. Licensing Architecture

### 6.1 Auth: Supabase magic link

- Email-only magic link at launch. No passwords.
- Sign in with Apple in v1.1 (~6 weeks post-launch).
- Skip Google (Mac-only product, negligible lift).
- Auth + entitlements + proxy rate-limit counters all on one Postgres = one vendor, one bill, one dashboard, RLS for free.

### 6.2 Entitlement delivery: Ed25519 JWT + 24h refresh + 7d offline grace

Short-lived JWT signed by the entitlement server. Public key baked into the binary at compile time.

```json
{
  "sub": "user_uuid",
  "tier": "pro" | "trial" | "free",
  "trial_ends_at": "2026-05-05T00:00:00Z",
  "entitlements": ["proxy", "cloud_models", "priority_asr", "history_export", "snippets_unlimited"],
  "iat": 1745260000,
  "exp": 1745346400,
  "offline_grace_until": 1745865200
}
```

**Behavior:**

| State | Network | Action |
|---|---|---|
| JWT fresh (<24h) | any | Use cached |
| JWT stale (24h–7d) | online | Refresh; on fail use cached |
| JWT stale (24h–7d) | offline | Use cached, subtle "offline" chip |
| JWT past 7d grace | any | Downgrade to Free, surface reconnect prompt |
| Refresh 401 | online | Clear tokens, prompt re-login |

**Revocation:** 24h for online users, 7d for offline. Acceptable for a $9 product.

### 6.3 Client-side layout

New crate `crates/rekody-license/`:

```
src/
├── mod.rs           // public API: current_tier(), refresh(), on_launch()
├── keychain.rs      // wraps `keyring` crate; service = "com.rekody.license"
├── token.rs         // JWT parse + Ed25519 verify via jsonwebtoken
├── refresh.rs       // tokio task: every 24h, exponential backoff
├── state.rs         // Tier enum + in-memory cache (tokio::sync::RwLock)
├── grace.rs         // offline window evaluation
└── errors.rs
```

- Refresh token + current JWT in Keychain (not a file — avoids iCloud sync leaking across Macs).
- Last-known-good claims cached at `~/Library/Application Support/rekody/entitlements.json` for cold-start before Keychain round-trip; signature re-verified on load.
- **Never crash, never block the dictation hotkey, never modal-block on licensing.** A licensing subsystem that breaks the core app is worse than piracy.

### 6.4 Anti-piracy stance

The binary is MIT. Anyone can fork, strip the license check, and ship a free build. That's the deal we made.

**The proxy is the product.** A cracked binary cannot call our Deepgram proxy without a server-issued JWT tied to a paying account. An OSS fork can point at Deepgram directly — which is exactly what the BYOK Free tier already allows.

No obfuscation, no anti-debug, no binary integrity checks. Honest customers pay; pirates never would. Don't burn weeks on DRM theater. Prior art: Sublime Text, TablePlus, Raycast, Superwhisper.

---

## 7. Billing

### 7.1 Provider decision — **OPEN (see §14)**

Two subagents gave conflicting recommendations:

- **Licensing agent:** Stripe + Stripe Tax (0.5% on tax). Flexibility, ecosystem, built-in proration, best webhook tooling.
- **Ops agent:** Lemon Squeezy as Merchant-of-Record. Handles EU VAT + US sales tax for a solo founder, removes real operational pain at ~5% + $0.50.

**Recommendation for this spec:** **Lemon Squeezy for launch**, revisit Stripe direct at ≥$10k MRR when the VAT filing load becomes worth absorbing in exchange for lower fees. Solo founder + global subscriptions + VAT MOSS in 27 countries = the 5% fee is cheap. If we go Stripe, we're buying flexibility we won't use for 12 months and owing VAT returns from month one.

### 7.2 Webhook → license refresh flow

```
Provider Event
   │
   ▼
Worker /webhooks/{provider}  ── verify signature
   │
   ▼
Enqueue to Postgres outbox (idempotency + replay)
   │
   ▼
Update `subscriptions` table:
   - checkout.completed     → tier = pro, current_period_end
   - invoice.paid           → extend current_period_end
   - invoice.payment_failed → mark past_due, send email
   - subscription.deleted   → tier = free at period_end (grace)
   │
   ▼
Publish realtime row change via Supabase Realtime
   │
   ▼
App receives push; requests fresh JWT
```

**Handle every webhook idempotently** keyed on `event.id`. **Never trust webhook order** — always reconcile against `subscriptions.retrieve()`. Build a **daily reconciliation job** that pulls full subscription state and corrects DB drift. Ship this **before launch, not after the first angry email**.

### 7.3 Cancellation UX

1. Pro stays active until `current_period_end`. No instant downgrade.
2. 7-day post-expiry grace in Free tier; one-click reactivation + short NPS-style "what would have kept you?" prompt.
3. After grace, **transparent switch to BYOK mode**. No feature loss beyond the proxy.

**On refund (14-day, no questions):** entitlement revoked within 24h; local data untouched — ever.

**On account deletion:** server-side purge within 30d (GDPR); local data stays until user deletes the app.

Cancellation lives inside the app: `Preferences → Account → Manage subscription` deep-links to provider's Customer Portal. No dark patterns, no retention surveys, no hidden settings.

---

## 8. API-Key Proxy Service

### 8.1 Stack

| Layer | Choice | Why |
|---|---|---|
| Runtime | **Rust + Axum on Fly.io** | Streaming WebSocket → Deepgram, low RAM, matches app stack. Fly anycast keeps latency low. |
| Edge | Cloudflare (proxied) | DDoS, cached entitlement endpoints, cheap bandwidth |
| State | Supabase Postgres + Upstash Redis | Postgres = ledger of truth; Redis = hot per-user counters w/ TTL |
| Secrets | Fly secrets | Provider keys never leave the server |

**Alternative considered:** Hono on Vercel Edge (simpler deploy, JS stack). Rejected because long-lived Deepgram WebSocket streaming is awkward on Workers/Edge and CPU-time-billed. **Worth revisiting** if ops overhead of Fly+Redis proves prohibitive for a solo dev (see §14).

### 8.2 Per-user budget enforcement

- Every request preceded by `INCRBYFLOAT user:{id}:usage_cents_mtd <estimated_cost>` in Redis.
- Over threshold → reject with specific error code; client translates to "You've hit your monthly cap; using local transcription."
- Daily rollup to Postgres for reporting.
- Admin kill-switch flag per user (abuse response without app update).

### 8.3 Unit-economics levers (post-launch)

1. Route to cheaper provider when possible (Groq Whisper-large-v3 for batch, Deepgram for streaming).
2. Cache common deltas (reformat-last-paragraph polish calls cache-keyed on input hash).
3. Ship a metered tier later if ~5% of users blow past cap — "$9 base + usage," Linear-style.

---

## 9. Signing, Notarization, Updates

### 9.1 Apple Developer Program

- **Enroll now as Individual ($99/yr).** Transfer to LLC Organization after company formation (§10). Transfer ships a new Developer ID cert; old builds stay valid until expiry. Plan transfer for a minor version bump (e.g. 0.6.0) so the cert change goes in release notes.
- Certificates: **Developer ID Application** (for `.app` + binaries). Skip Installer cert (no .pkg).
- **App Store Connect API key** for `notarytool` (preferred over Apple-ID-password auth).

### 9.2 Hardened runtime entitlements

Minimum set, save as `build/entitlements.plist`:

```xml
<key>com.apple.security.device.audio-input</key><true/>
<key>com.apple.security.network.client</key><true/>
<key>com.apple.security.cs.disable-library-validation</key><true/>
<!-- allow-jit only if whisper.cpp GGML JIT requires it; audit first -->
```

**`Info.plist`** needs `NSMicrophoneUsageDescription` (required for notarization). Accessibility is a **runtime TCC permission**, not an entitlement — nothing to declare, but the app must detect denial on wake via `AXIsProcessTrusted()` and deep-link to System Settings.

Ship only entitlements we use. Each extra is a support/PR ticket.

### 9.3 `notarytool` flow

```bash
xcrun notarytool store-credentials "rekody-notary" \
  --key ~/.private_keys/AuthKey_XXXX.p8 \
  --key-id XXXXXXX --issuer YYYYYYYY-...

# Per release:
codesign --force --options runtime --timestamp \
  --entitlements build/entitlements.plist \
  --sign "Developer ID Application: Tony Kipkemboi (TEAMID)" \
  "build/rekody.app/Contents/MacOS/rekody"
codesign --force --options runtime --timestamp \
  --sign "Developer ID Application: ..." "build/rekody.app"
ditto -c -k --keepParent build/rekody.app build/rekody.zip
xcrun notarytool submit build/rekody.zip --keychain-profile "rekody-notary" --wait
xcrun stapler staple build/rekody.app
xcrun stapler staple build/rekody.dmg   # staple DMG separately; DMG stapling doesn't staple .app inside
```

### 9.4 CI secrets

| Secret | Content |
|---|---|
| `MACOS_CERT_P12_BASE64` | `base64 -i DeveloperID.p12` |
| `MACOS_CERT_PASSWORD` | .p12 passphrase |
| `KEYCHAIN_PASSWORD` | random, throwaway CI keychain |
| `NOTARY_APPLE_ID` | Apple ID email |
| `NOTARY_API_KEY_P8_BASE64` | base64 of App Store Connect .p8 |
| `NOTARY_API_KEY_ID` | 10-char key ID |
| `NOTARY_API_ISSUER_ID` | UUID |
| `UPDATER_PRIVATE_KEY` | minisign/Tauri updater private key |
| `UPDATER_KEY_PASSWORD` | updater key passphrase |

### 9.5 Update mechanism

**Use `tauri-plugin-updater`** once we wrap in Tauri v2. Minisign signature check, differential updates, manifest-driven.

```json
{
  "plugins": {
    "updater": {
      "active": true,
      "endpoints": ["https://updates.rekody.com/{{target}}/{{current_version}}"],
      "dialog": true,
      "pubkey": "..."
    }
  }
}
```

**Manifest hosting:** Cloudflare R2 behind a Workers route (zero egress, DMGs are ~100MB). Worker validates requester's JWT before returning platform URL — enables paid-channel-only updates without forking the app.

**Rollback:** Worker reads "current pointer" from KV (`stable:darwin-aarch64 → 0.6.1`). Bad release → `wrangler kv:key put stable:darwin-aarch64 0.6.0`. Instant rollback. Keep last 2 DMGs + manifests in R2 indefinitely under `/dl/<version>/...`.

### 9.6 Distribution channels

| Channel | Verdict | Notes |
|---|---|---|
| Direct `.dmg` from `rekody.com` | **Ship at v1** | R2 + redirect; full control, analytics, paid-gate-able |
| Homebrew **cask** (for GUI `.app`) | Add at v1 | Keep existing formula for CLI users; new cask for desktop app |
| **Mac App Store** | **Defer indefinitely** | Sandbox blocks Accessibility API. Not viable. |
| Setapp | Revisit month 6+ | Apply after validating direct-sale churn |

---

## 10. Legal & Infra

### 10.1 Company formation

**Single-member LLC, Wyoming or Delaware.** ~$300 setup, $50–$300/yr. Sole prop = personal liability for every injection-caused data-loss claim. The $300 is worth it. Switching payment providers later mid-subscription is painful, so form the LLC **before** billing starts.

**Timeline:** form LLC → open Mercury bank → EIN from IRS (free, 10 min) → Lemon Squeezy / Stripe signup → enroll Apple Developer as Organization. Total ~2 weeks; start now.

### 10.2 Required legal docs

Updated 2026-04-22. Initial pass shipped in-house rather than through iubenda/Termly — see §10.6 for what's live.

| Doc | Status | Path | Notes |
|---|---|---|---|
| Privacy Policy | **Live** | `/privacy` | 14 sections; two-product framing (CLI vs Pro); sub-processor table references `/subprocessors` |
| Terms of Service | **Live** | `/terms` | 20 sections; California governing law; $100 / 12-mo-fees liability cap; 14-day no-questions refund |
| Security page | **Live** | `/security` | Zero-retention, encryption, code-signing, vuln disclosure — reviewed against §1.5 (no Ed25519/JWT/refresh-window details in public copy) |
| Data Controls | **Live** | `/data-controls` | Shortest-path page for export/delete/diagnostics opt-out. Response SLAs: 3-day ack, 30-day substantive |
| Sub-processors list | **Live** | `/subprocessors` | Current vendors only (Vercel/GitHub/HF/Homebrew). Planned vendors intentionally NOT disclosed — see §1.5 rule 2 |
| DPA template | Pending | — | Draft before first enterprise inquiry. Don't publish; send on request |
| Accessibility statement | Pending | `/accessibility` | Ship alongside home-page Pro retool. VoiceOver-first claim in `/use-cases` copy needs formal statement to back it |
| Cookie banner | Not needed | — | Website sets no tracking cookies; privacy-friendly analytics via Vercel |

Lawyer review pending — schedule before any paid marketing. No iubenda/Termly boilerplate for the shipped pages; they're bespoke and will need attorney sign-off or replacement.

**Policy:** any new customer-facing legal page passes §1.5 review before publish — no named planned vendors, no named crypto algorithms, no architectural step counts, no BYO evangelism.

### 10.3 DNS / infra

| Record | Points to | Purpose |
|---|---|---|
| `rekody.com` | Vercel | Marketing site (existing) |
| `www.rekody.com` | → apex | Redirect |
| `updates.rekody.com` | CF Worker + R2 | Update manifest + DMG hosting |
| `api.rekody.com` | Fly.io (or Worker) | Auth + entitlements + proxy |
| `status.rekody.com` | Instatus (free) | Status page — 5 components covers it |
| `rekody.app` | → rekody.com | Defensive redirect |

### 10.4 Support addresses

| Address | Status | Action |
|---|---|---|
| `security@rekody.com` | Referenced in SECURITY.md; **routing unverified** | Configure CF Email Routing → `iamtonykipkemboi@gmail.com`. **Test before any paid marketing.** |
| `support@rekody.com` | Doesn't exist | Create; route to same inbox; split when >20/week |
| `hello@rekody.com` | — | Optional, for press |

### 10.5 In-app diagnostics (new `rekody diagnose` command)

Bundle: `rekody --version`, macOS version, arch; last 200 lines of `~/Library/Logs/rekody/rekody.log` run through `sanitize()`; permission status (mic, Accessibility via `AXIsProcessTrusted()`); selected providers (**never** API keys); user-entered description. POST to `api.rekody.com/report` → GitHub Issue (OSS bug) or email (account issue) based on severity flag.

### 10.6 Website & legal surface status (as of 2026-04-22)

**Live at rekody.com:**

| Route | Purpose | Notes |
|---|---|---|
| `/` | Home | OSS-product-facing today; retool to Pro-first is a Phase 3 item |
| `/use-cases` | 9 professions + accessibility callout + multilingual panel | Shipped; SEO-friendly `/use-cases` path |
| `/open-source` | OSS pitch: 6-crate breakdown, install tabs, MIT callout, contribute CTAs | Shipped; becomes the OSS landing when home retools to Pro-first |
| `/privacy` | Privacy Policy | Shipped; see §10.2 |
| `/terms` | Terms of Service | Shipped; see §10.2 |
| `/security` | Security page | Shipped; reviewed against §1.5 (generic wording, no impl details) |
| `/data-controls` | DSR self-serve + in-app control map | Shipped |
| `/subprocessors` | Current vendors only | Shipped; see §1.5 rule 2 |
| `/install.sh` | Real installer copied from repo root | Shipped |

**Pending before Pro launch (blocking):**
- `/pricing` — paid conversion page. No design yet. Design task: match Fraunces/teal language; two cards (Free, Pro) plus Teams placeholder; no feature-matrix-as-shame pattern.
- `/docs` — support surface. Needs content plan + information architecture. Defer to Phase 2.
- `/blog` — referenced by `/security` incident-response copy ("post-incident writeups on our blog"). Either ship or rewrite the security clause.
- Home-page retool to Pro-first — Phase 3a per roadmap.

**Pending but non-blocking:**
- `/accessibility` — accessibility statement. Ship alongside home retool. Our `/use-cases` copy claims VoiceOver support and hands-free modes; these need a formal statement to back.
- `/changelog` — currently links to GitHub release notes; acceptable.
- `/status` — Instatus page. Pointless until Pro has an uptime story.
- `/download` — duplicates `/#install`; skip.

**Public-comms discipline (§1.5) acceptance criteria for any new page:**
1. No named planned sub-processors.
2. No named crypto algorithms or protocol specifics.
3. No numbered architecture walk-throughs.
4. No "BYO cut us out of the data path" framing.
5. Trust signals (zero-retention, encryption, signing) explicit and plain.

---

## 11. Telemetry

**Stance:** opt-in only, default **off**, matches existing SECURITY.md.

**Vendor:** self-hosted **GlitchTip** ($6/mo Hetzner or DO droplet) for crashes + **Counterscale** on Cloudflare Workers ($0–$5/mo) for website analytics. Both GDPR-clean, aligns with privacy pitch.

**New crate** `crates/rekody-telemetry/` with `emit(event: TelemetryEvent)`. Ring buffer locally; flushes over HTTPS only if `preferences.telemetry = "enabled"`. First-run consent card in onboarding.

**Event allowlist (hardcoded):**
- `app.startup_failed` (error type, no paths)
- `app.panic` (Rust backtrace, sanitized)
- `stt.provider_error` (provider name, HTTP status — **no prompt, no audio**)
- `hotkey.conflict` (keycode only)
- `update.check_failed` (HTTP status)
- `injection.failed` (reason code — **never** target bundle ID)

**Single `sanitize()` function:**

```rust
fn sanitize(s: &str) -> String {
    s.replace(env::var("HOME").unwrap_or_default().as_str(), "~")
     .replace(whoami::username().as_str(), "<user>")
     .replace(regex!(r"/Users/[^/\s]+"), "/Users/<user>")
}
```

Unit-test with PII fixtures. **Never** ship audio, transcripts, dictionary entries, snippet contents, or foreground bundle IDs.

**Local logs:** `~/Library/Logs/rekody/rekody.log`, daily rotation via `tracing-appender`, keep 7 days.

---

## 12. Tauri v2 Migration Plan

This is the single largest piece of new work and blocks everything designed in Paper.

### 12.1 Target layout

```
chamgei/
├── apps/
│   └── desktop/
│       ├── src-tauri/          (Tauri v2 shell, references existing crates)
│       └── src/                (React 19 + Vite + Tailwind 4)
├── crates/
│   ├── rekody-core/            (existing)
│   ├── rekody-audio/           (existing)
│   ├── rekody-stt/             (existing — split into providers/)
│   ├── rekody-llm/             (existing — split into providers/)
│   ├── rekody-inject/          (existing)
│   ├── rekody-hotkey/          (existing)
│   ├── rekody-ipc/             (NEW — DTOs + tauri-specta bindings)
│   ├── rekody-license/         (NEW)
│   └── rekody-telemetry/       (NEW)
├── services/
│   ├── api/                    (Hono + Neon Postgres + Stripe/LS webhook)
│   └── proxy/                  (Rust Axum on Fly — OR — Hono edge, see §14)
└── website/                    (existing Astro, add pricing + legal + docs)
```

### 12.2 IPC contract layer

`crates/rekody-ipc/` owns every `#[derive(Serialize, Deserialize, specta::Type)]` DTO. `tauri-specta` emits `apps/desktop/src/bindings.ts`. One source of truth on the wire. Without typed bindings, every front-end bug becomes a runtime mystery.

### 12.3 Workspace hygiene pass (alongside the Tauri scaffolding)

- `rust-toolchain.toml` pinning `1.85.0`
- `.cargo/config.toml` for target dirs
- `deny.toml` for `cargo-deny`
- Unified `[workspace.lints]` in root `Cargo.toml`
- Replace ad-hoc `anyhow` at public crate boundaries with `thiserror` enums; `anyhow` stays inside binaries only
- `cargo audit` + `cargo deny` gating in CI

### 12.4 Surface-by-surface scope

Each page = **Rust IPC command(s) → typed binding → React route**.

| Surface | Backend work | Frontend files | License flag |
|---|---|---|---|
| **Dictate** | `get_today_stats`, `get_recent_dictations`, `tauri::Event` channel `dictation://partial` | `routes/dictate/{LiveStrip,DictateSurface,TodayStats,RecentList}.tsx` | none (must work Free) |
| **History** | extend `history.rs`: `list_grouped_by_day`, `search`, `get_audio_path`, `export(Csv\|Json\|Md)`. Audio as Opus at `~/Library/Application Support/rekody/audio/YYYY-MM-DD/{uuid}.opus`, 30d retention | `routes/history/{index,DayGroup,ExpandedRow,Filters,AudioPlayer,ExportModal}.tsx` | Export gated on `history_export` |
| **Dictionary** | extend `dictionary.rs`: `list_pending_suggestions`, `accept_suggestion`, `generate_phonetic_hint`. Suggestions fed by post-dictation edit diffs (`corrections.rs`) | `routes/dictionary/*` | Free capped at 50 |
| **Snippets** | extend `snippets.rs`: `list_by_category`, `increment_usage`, dynamic variables in `rekody-inject` | `routes/snippets/*` | Free capped at 10 (`snippets_unlimited`) |
| **STT Engine** | split `rekody-stt` into `providers/{local_whisper,groq,deepgram,proxy}.rs` behind `trait SttProvider`; add `diagnose()` per provider | `routes/settings/stt/*` | "rekody Cloud" only if `stt_proxy` entitled |
| **Polish Engine** | split `rekody-llm` into 11 provider modules; `PromptStyle` enum in `prompts.rs`; `preview(sample, style, provider)` debounced at 400ms | `routes/settings/polish/*` | Proxy option gated on `polish_proxy` |
| **Hotkey** | `capture_next_chord`, `check_conflict(chord) -> Option<ConflictingApp>`, secondary shortcuts (pause/cancel/retry/open-history) | `routes/settings/hotkey/*` | none |
| **Preferences** | `get_permissions_state`, `export_all_data`, `delete_all_data` | `routes/preferences/{PlanCard,Account,Permissions,Privacy,DataExport}.tsx` | Plan card reads entitlements |
| **HUD** | separate borderless always-on-top Tauri window, 280×72, states: idle-hidden, listening, transcribing, polishing, injecting, error | `windows/hud.rs`, `routes/hud/*` | none |
| **Onboarding** | first-run flow; Welcome → Permissions → Hotkey+Test → Plan; magic-link trial kickoff | `routes/onboarding/*` | trial start |

### 12.5 Error surfaces

Formalize `rekody_core::Error`: `Permission(kind)`, `ProviderKey`, `Network`, `AudioDevice`, `Notarization`. Each → `ErrorToast` component with headline, one-line cause, primary action ("Open Accessibility settings"). All backend errors route through `tauri::ipc::InvokeError` with stable error codes → `apps/desktop/src/lib/errorMap.ts`.

---

## 13. Roadmap & Timeline

Solo + one part-time contractor. Dates are **earliest responsible**, not ambitious. Conservative buffer for Apple review friction, OSS interrupts, life.

| Phase | Milestone | Target date | Dominant work |
|---|---|---|---|
| **0** | Foundations complete | **2026-05-26** (5wk) | Tauri shell stand-up (weeks 1–3), signing/notarization CI (week 4–5), IPC/telemetry/workspace hygiene |
| **1** | Licensing live | **2026-06-23** (+4wk) | Magic-link auth, entitlement JWT, Keychain license storage, Stripe/LS webhook, customer portal, offline grace |
| **2** | All designed surfaces shipped | **2026-08-11** (+7wk) | 9 surfaces (§12.4) avg 4–5 working days each including design reconciliation |
| **3a** | Private beta start | **2026-08-18** (+1wk) | 1wk internal dogfooding gap |
| **3b** | Public paid launch | **2026-09-29** (+6wk) | 6-week beta, accessibility audit, pricing page, email templates, launch artifacts |
| **4** | Post-launch stabilization | Weeks 1–8 after launch | Weekly patch cadence weeks 1–4, biweekly weeks 5–8, monthly from week 9 |

**Slack:** ~2 weeks absorbed by a11y audit fixes, pricing iteration, notarization retries, dashboard wiring.

**Notarize weekly in CI starting Phase 0.** Keep `docs/notarization-runbook.md` with every error string hit + fix. This is the single most-likely-to-slip item.

### 13.1 Phase 0 breakdown (XL items called out)

| # | Item | Effort | Files |
|---|---|---|---|
| 0.1 | Tauri v2 shell + React 19/Vite/Tailwind 4 + port CLI commands to IPC | **XL** | `apps/desktop/src-tauri/*`, `apps/desktop/src/*` |
| 0.2 | `rekody-ipc` crate + tauri-specta bindings | M | `crates/rekody-ipc/*` |
| 0.3 | Workspace hygiene (toolchain, deny, lints, error types) | S | root configs |
| 0.4 | CI with signing + notarization + DMG + appcast | L | `.github/workflows/release.yml`, entitlements.plist |
| 0.5 | Telemetry opt-in scaffold | M | `crates/rekody-telemetry/*`, onboarding consent card |
| 0.6 | API-key proxy skeleton | L | `services/proxy/*` |
| 0.7 | Error surfaces + ErrorToast | M | `crates/rekody-core/src/error.rs`, `apps/desktop/src/components/ErrorToast.tsx` |

### 13.2 Private beta (Phase 3a)

- ~20 invitees: HN who-is-hiring replies on existing OSS, personal network, Guild peers, 2 a11y-focused power users.
- Invite-only via signed license, `tier = pro_beta`, `trial_ends_at = launch_date + 30d`.
- `#rekody-beta` Discord, weekly 30-min office hours.
- Daily Tinybird dashboard review first 10 days: dictations/user/day, error rate by provider, p95 latency, crashes/session. Slack webhook alert if error rate >5%.

### 13.3 Public launch (Phase 3b)

- **Product Hunt:** gallery (6 images: hero + 5 features), 60s demo video (ScreenFlow, one afternoon), first comment drafted w/ privacy stance up top. Schedule Tuesday 00:01 PT.
- **HN:** "Show HN: rekody — privacy-first voice dictation for macOS (BYOK + Cloud option)" 08:00 PT same day. No marketing tone. Link repo, pricing, proxy architecture doc.
- Pricing page live at `website/src/pages/pricing.astro`; Free/OSS path not hidden.
- Changelog at `/changelog` sourced from `website/src/content/changelog/*.mdx`.
- Email templates (Resend + React Email) wired to Vercel cron.

---

## 14. Open Decisions

These are where the five subagents disagreed or where the data doesn't yet justify a firm call. Pick before Phase 1.

### 14.1 Billing: Stripe vs. Lemon Squeezy

- **Stripe direct** (licensing agent): 2.9% + $0.30, Stripe Tax +0.5%, best tooling, full flexibility. You are Merchant of Record → you file VAT in every jurisdiction you sell into.
- **Lemon Squeezy** (ops agent): 5% + $0.50, MoR handles EU VAT + US sales tax. ~$0.24 more per transaction at $9; trades ~2.6% margin for zero VAT operational load.

**Spec recommendation:** Lemon Squeezy for launch. Revisit Stripe at ≥$10k MRR.

### 14.2 Proxy host: Fly.io (Axum) vs. Vercel Edge (Hono)

- **Fly.io + Axum** (licensing agent): Rust, WebSocket-native, matches app stack, manual ops surface (Fly deploys, Redis on Upstash, CF in front).
- **Vercel Edge + Hono** (roadmap agent): JS, simpler deploy, one vendor alongside the Astro site, more awkward for long-lived streaming WebSockets.

**Spec recommendation:** Start with Vercel Edge + Hono for Phase 0 skeleton (faster to ship). Move STT streaming to Fly+Axum in Phase 2 if Deepgram WebSocket latency on Edge proves unacceptable (measure before moving).

### 14.3 Auth: Supabase vs. Neon + DIY

- **Supabase** (licensing agent): auth + DB + Realtime + RLS all one vendor. Free tier generous; Pro $25/mo.
- **Neon Postgres + Resend + DIY magic link** (roadmap agent): cheaper (~$15/mo), more code you own.

**Spec recommendation:** Supabase. Solo-dev lift matters more than $10/mo at this stage.

### 14.4 Trial abuse threshold

Soft device hash + flag on ≥2 prior trials. No auto-block at launch. **Open:** revisit after 30 days of real data. If abuse >5%, tighten (CC required, harder hash). If <2%, keep lenient.

### 14.5 Soft cap (1,000 min/mo)

Guess. If <5% of Pro users approach it after month 2, raise to 1,500 and use as marketing. If >15% hit it, fix is faster local-fallback UX, not a lower cap or higher price. **Hold $9 for 12 months regardless** — price is part of positioning vs. Superwhisper ($18) and shouldn't flex.

---

## 15. Pre-Launch Hardening Checklist

- [ ] Keychain items use `kSecAttrAccessGroup = <TEAMID>.com.rekody.app` + `kSecAttrAccessible = WhenUnlockedThisDeviceOnly`. ACL via `SecAccessCreateWithOwnerAndACL` restricts to signed binary.
- [ ] Entitlements audited: no `allow-jit`, no `disable-executable-page-protection`, no `get-task-allow` (dev-only).
- [ ] `grep -r "api_key\|api_token\|bearer" crates/` clean. Every log site uses `secrecy::Secret<String>` whose `Debug` prints `[REDACTED]`.
- [ ] Updater pubkey baked into binary at compile time (`env!("TAURI_SIGNING_PUBLIC_KEY")`), verified at runtime.
- [ ] Minisign signature check happens **before** tarball extraction.
- [ ] `SHA256SUMS.minisig` shipped alongside `SHA256SUMS`.
- [ ] Tauri `app.security.csp` set strict; `dangerousDisableAssetCspModification = false`.
- [ ] `cargo audit` + `cargo deny` green.
- [ ] `telemetry::sanitize()` unit-tested against PII fixtures.
- [ ] `security@rekody.com` routing verified end-to-end.
- [ ] `dist/chamgei-*` filenames renamed to `dist/rekody-*`; existing Homebrew formula bumped in same release.
- [ ] Daily webhook reconciliation job running in staging for ≥7 days before prod.
- [ ] GlitchTip live ≥24h w/ zero PII leaks under load test.
- [ ] Runbooks exist (not stubs) at `docs/runbooks/{cert-revoked, update-endpoint-compromise, cve-advisory, webhook-drift, deepgram-outage}.md`.

---

## 16. Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Apple notarization friction | **High** | High | Notarize weekly in CI from Phase 0. Living runbook of every error + fix. |
| Deepgram/Groq pricing change mid-launch | Medium | High | Multi-provider fallback in proxy. Per-user metering from day 1. Contracts w/ 2 providers. |
| Accessibility API TCC silently revoked after OS update | **High** | Medium | Detect `AXIsProcessTrusted()` on wake; blocking modal w/ deep-link `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`. |
| Cursor injection edge cases (Electron, Chrome, VS Code, Figma desktop) | **High** | Medium | Per-app `inject_strategy` table (CGEvent synthetic / pasteboard round-trip / AppleScript whitelist). Manual smoke grid per release. |
| Trial abuse | Medium | Low | Soft hash + flag. Budget ~2% as marketing cost. Don't overfit. |
| Webhook drift → stuck `past_due` accounts | Medium | Medium | Daily reconciliation job before launch. |
| Offline grace UX confusion (traveler at day 5) | **High** | Low | Persistent chip + day-5 notification + plain-English explanation. Most likely support-ticket generator. |
| OSS-community backlash about paid tier | Medium | Medium | Pre-empt w/ signed blog post: MIT stays, BYOK stays free. Obsidian + Raycast playbooks. Respond in kind, not panic. |
| Support volume overwhelms solo founder | **High** | Medium | In-app `Report issue` auto-attaches sanitized logs. Auto-triage by provider tag. Troubleshooting doc before launch. |
| Competitor undercut at $5/mo | Medium | Medium | Compete on privacy + BYOK + OSS lineage, not price. Pre-write "why $9" post. $79 annual ready. |
| Whisper.cpp regressions on new Apple silicon | Low | Medium | Pin known-good version; CI smoke on M-series. |
| Cert revoked by Apple | Low | High | Runbook: spctl detect → re-sign/notarize last 2 releases → CF cache bust → banner + status incident. Existing installs keep working (Gatekeeper checks at install/first-launch only). |

---

## 17. First-Week-of-Launch Runbook

**Continuous watch:**

- Tinybird dashboard tab pinned: error rate by provider, p95 latency, dictations/user
- GlitchTip for panics; alert on any uncaught panic
- Billing dashboard: checkout conversion, failed payments
- HN/PH comment queues: 15-min refresh first 12h, hourly after
- `#rekody-beta` Discord, `support@rekody.com` inbox
- GitHub issues + Homebrew tap PRs (OSS users file here first)

**P0 bug reported (ship within 24h):**

1. Reproduce with reporter's config (ask for `~/.config/rekody/config.toml` w/ keys scrubbed + `rekody doctor` output).
2. Open GitHub issue even for closed-beta bugs — visibility matters.
3. Branch `hotfix/v1.0.x`, fix, regression test under `crates/rekody-*/tests/` or `apps/desktop/tests/`.
4. Tag → CI builds signed+notarized DMG → auto-updater picks it up → post in Discord + status update.

**HN/PH etiquette:**

- Don't DM strangers.
- Reply substantively to every top-level comment in the first 4 hours.
- Factual errors: correct without defensiveness.
- "Overpriced" feedback: thank + link pricing rationale post.
- Legitimate DMs: 2–3 personally-asked-to-be-pinged contacts only. No broadcast.

**Triage buckets:**

| Type | Response |
|---|---|
| `bug` (reproducible) | GitHub issue within 2h |
| `UX` (works but confusing) | Linear ticket + screenshot |
| `feature` (out of v1 scope) | Thank-you + link `/roadmap` |
| `perception` (misunderstood) | One founder reply, calm, factual. No thread war. |

---

## 18. Launch-Day Metrics Dashboard

Target ranges (adjust after 30 days of real data):

| Metric | Target | Alarm |
|---|---|---|
| Trial → Paid conversion | 8–12% | <5% after day 30 |
| D7 retention (paid) | >80% | <65% |
| D30 retention (paid) | >70% | <55% |
| Dictations / DAU | >10 | <3 |
| Refund rate | <5% | >10% |
| Support tickets / 100 active users / week | <3 | >8 |
| 429 rate from providers | <2% | >5% |
| Per-user monthly COGS | <$5.40 on Pro | any user >$10 → kill switch review |

---

## 19. Relevant File Paths

**Existing:**

- Workspace: `/Users/tonykipkemboi/Startups/chamgei/Cargo.toml`
- Crates: `/Users/tonykipkemboi/Startups/chamgei/crates/rekody-{core,audio,stt,llm,inject,hotkey}/src/`
- CI (needs rewrite): `/Users/tonykipkemboi/Startups/chamgei/.github/workflows/release.yml`
- Install script: `/Users/tonykipkemboi/Startups/chamgei/install.sh`
- Security policy: `/Users/tonykipkemboi/Startups/chamgei/SECURITY.md`
- Website: `/Users/tonykipkemboi/Startups/chamgei/website/`
- Onboarding spec: `/Users/tonykipkemboi/Startups/chamgei/docs/onboarding-spec.md`

**To create:**

- `apps/desktop/src-tauri/` — Tauri v2 shell
- `apps/desktop/src/` — React 19 + Vite + Tailwind 4
- `crates/rekody-ipc/` — typed IPC DTOs
- `crates/rekody-license/` — JWT refresh + Keychain + offline grace
- `crates/rekody-telemetry/` — opt-in event pipeline
- `services/api/` — auth + entitlements + webhook
- `services/proxy/` — managed STT + Polish proxy
- `apps/desktop/src-tauri/entitlements.plist`
- `apps/desktop/src-tauri/tauri.conf.json` (updater config, bundle identity)
- `rust-toolchain.toml`, `.cargo/config.toml`, `deny.toml`
- `docs/runbooks/{cert-revoked,update-endpoint-compromise,cve-advisory,webhook-drift,deepgram-outage}.md`
- `website/src/pages/pricing.astro`
- `website/src/pages/legal/{privacy,terms,subprocessors}.mdx`
- `website/src/content/changelog/`
- `website/src/content/docs/`
- `scripts/bump-version.sh`, `scripts/release.sh`

---

## 20. Positioning Anchor (copy for reference)

**Headline (Fraunces):** *Dictation that listens carefully.*

**Sub (Source Serif 4 italic):** *rekody turns speech into clean text — and then into the right kind of clean text for where it's going. A clinical note sounds different from a Slack DM. Your words should too.*

**Positioning:** *rekody is open source and works forever for free with your own API keys. Pro is for people who'd rather not manage keys, want their dictionary and profiles on every Mac they own, and need the register to shift automatically between the chart, the pull request, and the group chat. Fourteen days, no card, cancel in one click.*

---

*End of spec. Revisit §14 (open decisions) with the team before starting Phase 1. Everything else is executable as written.*
