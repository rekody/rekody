# rekody — project notes for Claude

## Release & rollback surface

A rekody release is **NOT** a single artifact. Pushing tag `vX.Y.Z` to `rekody/rekody` triggers `.github/workflows/release.yml`, which fans out to three places. Any release OR rollback MUST account for all of them — missing one leaves users on a broken or stale version.

### Where a version lives

| # | Location | What it is | How it's set on release |
|---|---|---|---|
| 1 | `Cargo.toml` (workspace root, `[workspace.package].version`) | Source of truth for `rekody --version` and the website's displayed version (`website/src/lib/version.ts` reads this file at build time) | Manual bump in a commit on `main` |
| 2 | Git tag `vX.Y.Z` on `rekody/rekody` | Triggers the release workflow | Manual `git tag` + `git push origin vX.Y.Z` |
| 3 | GitHub Release at `rekody/rekody/releases/tag/vX.Y.Z` | Hosts the `.tar.gz` binaries + `SHA256SUMS` | Auto-created by `release.yml` `create-release` job |
| 4 | Homebrew tap formula at `rekody/homebrew-rekody/Formula/rekody.rb` | What `brew install rekody` actually downloads — has `version` field + arm64/x86_64 SHA256s | Auto-updated by `release.yml` `update-homebrew-tap` job (commits to the tap repo using `PAT_HOMEBREW`) |
| 5 | (downstream) User's local Homebrew install | Cached formula + installed binary | User runs `brew update && brew upgrade rekody` |

### Releasing — full checklist

1. Bump `version` in workspace `Cargo.toml`, commit on `main`.
2. `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. Watch `release.yml` — it must build both arches, create the GitHub Release, AND update the tap formula. Confirm the tap commit lands at `rekody/homebrew-rekody`.
4. `brew update && brew info rekody` should show the new version. Test `brew upgrade rekody` end-to-end before declaring done.

### Rolling back — full checklist (THE PART THAT BIT US)

Reverting a bad release means undoing **everything** the release workflow created. There is no inverse automation; each item is manual.

1. **Code on `main`**: revert the offending commit(s) with `git revert` (non-destructive). Bump `Cargo.toml` back if needed and push.
2. **Local tag**: `git tag -d vX.Y.Z`.
3. **Remote tag**: `git push origin :refs/tags/vX.Y.Z`.
4. **GitHub Release**: `gh release delete vX.Y.Z --yes` (the release does NOT disappear when the tag does).
5. **Homebrew tap formula** ⚠️ EASY TO FORGET: revert the auto-commit in `rekody/homebrew-rekody`. Clone it, `git revert <sha>`, push. Without this, `brew install rekody` still pulls the broken version. Verify with `gh api repos/rekody/homebrew-rekody/contents/Formula/rekody.rb --jq .content | base64 -d`.
6. **Local install**: `brew update && brew uninstall rekody && brew install rekody`, then `rekody --version` to confirm. The tap is cached locally — `brew update` is required to pick up the formula revert.

### Verify-everywhere command

Before declaring a release OR a rollback complete, run:

```bash
# Main repo state
gh release list --repo rekody/rekody --limit 3
git ls-remote --tags origin | grep vX.Y.Z

# Tap state (this is the one that got missed)
gh api repos/rekody/homebrew-rekody/contents/Formula/rekody.rb --jq .content | base64 -d | grep -E '^\s+version'

# Local install
brew info rekody | head -3
rekody --version
```

If any of those four don't agree, the release/rollback is incomplete.

### Rollback automation

The forward path is automated (`release.yml`). For the reverse path, run:

```bash
scripts/rollback.sh <version>   # e.g. scripts/rollback.sh 0.5.5
```

That covers steps 2–5 of the rollback checklist (tag, GH release, tap, verify). Step 1 (reverting the version-bump commit on `main`) is intentionally left manual — review it with `git revert` so you can see the diff. The script is idempotent: re-run it safely if part of a rollback already happened.

## Other notes

- **`crates/rekody-core/src/history_tui.rs` is the gold-standard UI reference and must not be modified**, even for no-visual-change refactors. Duplicate its palette into other files rather than refactoring it into a shared module.
- Website lives at `website/`, deploys via Vercel, uses pnpm + Astro 6 (`astro ^6.4.8`; the site migrated to Astro 6, confirmed 2026-07-13). Build with the real toolchain (`pnpm build`); do not assume Astro 5.
- **CodeQL code scanning is enabled** on this repo (GitHub Actions security + quality scanning, activated 2026-07-13). Treat its findings as work: check open alerts as part of any substantial change or release (`gh api repos/rekody/rekody/code-scanning/alerts --jq '.[] | {number, rule: .rule.id, severity: .rule.security_severity_level, state, path: .most_recent_instance.location.path}'`), fix the real ones rather than letting them accumulate, and dismiss false positives with an explicit reason. Do not ship a release with an unaddressed high/critical CodeQL alert without flagging it.
- **Rust toolchain is pinned in `rust-toolchain.toml`** (channel + clippy/rustfmt + cross-compile targets). CI and `release.yml` use `rustup show` to honor it, NOT `dtolnay/rust-toolchain@stable` — that floated and caused local clippy to pass while CI failed on a newer lint. To bump Rust: edit the channel in `rust-toolchain.toml` AND run `rustup update` locally so both move together. Don't reintroduce `@stable`.

## Brand & marketing site (decided 2026-07-09)

- **Naming:** "Rekody" (capital R) in ALL prose and marketing copy. Lowercase `rekody` only for: the logo/wordmark, CLI commands (`brew install rekody`, `rekody update`), URLs, and crate names.
- **Color:** pure white (#FFFFFF) page grounds + #FAFAFA section bands — never warm cream page backgrounds. Cream #FBFAF4 (`--color-paper`) lives only inside dark surfaces (pill, receipt card) and the app-icon glyph.
- **HUD/pill representations anywhere** (site, mocks, demos) must animate: moving waveform bars + blinking caret (and ideally a ticking timer), all guarded by `prefers-reduced-motion`. Never render the pill frozen. Match the real `PillView.swift` anatomy (hands-free state: app icon → hollow teal circle + teal timer → "tap ⌥ + space to stop" → waveform → dim→bright transcript tail → caret).
- **Claims must be repo-verified before publishing.** Current truths: latency ~100ms after release (measured ~55ms streaming tail, `streaming.rs`); "0 network calls" is runtime-scoped — models download once at setup, updates are manual; binary is ~35 MB unstripped (the old "5 MB" claim was false — never resurrect it).
- **Do not market**: multilingual/100+ languages (until Whisper multilingual is tested and first-class) or Windows (until a public installable artifact ships — see WINDOWS.md gate).
- The landing page design source of truth is the "Direction A3 — Pure White" board in the Paper file "Rekody Branding" (app.paper.design/file/01KX3G2BD3FVCSY3ETHKXH8XR3, page "website").

## Benchmark publishing policy (decided 2026-07-10)

- Public benchmarks (rekody.com or any marketing surface) never include artifacts from individuals' accounts (e.g. third-party quantizations). Those are internal references only.
- Compare only against widely recognized models users know (whisper-large-v3, large-v3-turbo today; add others as rekody's lineup grows).
- Numbers must come from rekody-bench runs with the leaderboard-pinned methodology (normalizer + datasets), and only after the calibration gate passes.
- Benchmark visuals for the site are a deliberate design task, not an afterthought.
