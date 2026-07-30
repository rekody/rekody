#!/usr/bin/env bash
# review-e2e.sh — sandboxed end-to-end of the review product against a
# SYNTHETIC dataset: server contracts, a decision, the inbox counter, and
# an export cut, exercised through a real rekody binary exactly as a user
# would. No personal data, no daemon, no hotkeys — safe anywhere, CI
# included.
#
# usage: scripts/review-e2e.sh [path-to-rekody-binary]
#   default: target/release/rekody
set -euo pipefail

BIN="${1:-target/release/rekody}"
[[ -x "$BIN" ]] || { echo "error: binary not found: $BIN" >&2; exit 1; }
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

PORT=7891
TMP="$(mktemp -d)"
SRV_PID=""
cleanup() { [[ -n "$SRV_PID" ]] && kill "$SRV_PID" 2>/dev/null || true; rm -rf "$TMP"; }
trap cleanup EXIT

DS="$TMP/dataset"
mkdir -p "$DS/audio"

# ── Synthetic dataset: three 1 s tone WAVs + manifest rows ───────────────────
# (.wav paths: the store is extension-agnostic and this keeps the script
# independent of afconvert.)
python3 - "$DS" <<'PY'
import json, math, struct, sys, wave
ds = sys.argv[1]
for name, text, stamp in [
    ("a", "alpha one", "2026-07-01T10-00-00"),
    ("b", "bravo two", "2026-07-02T10-00-00"),
    ("c", "charlie three", "2026-07-03T10-00-00"),
]:
    with wave.open(f"{ds}/audio/{name}.wav", "wb") as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000)
        w.writeframes(b"".join(
            struct.pack("<h", int(8000 * math.sin(2 * math.pi * 440 * t / 16000)))
            for t in range(16000)))
with open(f"{ds}/manifest.jsonl", "w") as f:
    for name, text, stamp in [
        ("a", "alpha one", "2026-07-01T10-00-00"),
        ("b", "bravo two", "2026-07-02T10-00-00"),
        ("c", "charlie three", "2026-07-03T10-00-00"),
    ]:
        f.write(json.dumps({"audio_filepath": f"audio/{name}.wav", "text": text,
                            "duration": 1.0, "engine": "nemotron",
                            "timestamp": stamp}) + "\n")
PY

fail() { echo "review-e2e FAIL: $1" >&2; exit 1; }

# ── Server contracts ─────────────────────────────────────────────────────────
"$BIN" review --dir "$DS" --port "$PORT" --no-open --auto-exit-secs 0 \
  > "$TMP/stdout.log" 2> "$TMP/stderr.log" &
SRV_PID=$!

URL=""
for _ in $(seq 1 50); do
  URL="$(grep -m1 '^REVIEW_URL=' "$TMP/stdout.log" 2>/dev/null | cut -d= -f2- || true)"
  [[ -n "$URL" ]] && break
  kill -0 "$SRV_PID" 2>/dev/null || { cat "$TMP/stderr.log" >&2; fail "server died on startup"; }
  sleep 0.2
done
[[ "$URL" == "http://127.0.0.1:"* ]] || fail "REVIEW_URL contract broken: '$URL'"

PING_HDR="$(curl -s -o /dev/null -D - "$URL/api/ping" | tr -d '\r' | grep -i '^x-rekody-review:' || true)"
[[ -n "$PING_HDR" ]] || fail "/api/ping missing X-Rekody-Review header"

STATE="$(curl -s "$URL/api/state")"
echo "$STATE" | grep -q '"' || fail "/api/state returned nothing"

# ── Inbox counter in the page ────────────────────────────────────────────────
PAGE="$(curl -s "$URL/")"
echo "$PAGE" | grep -qi "awaiting review" || fail "page missing inbox counter copy"

# ── A decision lands and survives ────────────────────────────────────────────
curl -s -X POST "$URL/api/start" -d '{"second_opinion": false}' > /dev/null
DECIDE="$(curl -s -X POST "$URL/api/decide" \
  -d '{"clip": "audio/a.wav", "action": "edit", "final_text": "alpha one edited"}')"
echo "$DECIDE" | grep -q "alpha one edited" || fail "decision not applied: $DECIDE"
grep -q "alpha one edited" "$DS/manifest.jsonl" || fail "manifest not rewritten"
grep -c . "$DS/manifest.jsonl" | grep -qx 3 || fail "manifest row count changed"
[[ -f "$DS/decisions.jsonl" ]] || fail "decisions.jsonl not written"

kill "$SRV_PID"; wait "$SRV_PID" 2>/dev/null || true; SRV_PID=""

# ── Headless export cut ──────────────────────────────────────────────────────
# (REKODY_TRAINING_DIR, not --dir: the export subcommand shipped in 0.5.25
# without a --dir flag — found by this very script.)
EXPORT_OUT="$(REKODY_TRAINING_DIR="$DS" "$BIN" review export --copy-audio 2> "$TMP/export-err.log" || true)"
CUT="$(printf '%s\n' "$EXPORT_OUT" | grep -m1 '^EXPORT_PATH=' | cut -d= -f2- || true)"
[[ -n "$CUT" && -d "$CUT" ]] || { cat "$TMP/export-err.log" >&2; fail "EXPORT_PATH contract broken: '$EXPORT_OUT'"; }
EXTRA_LINES="$(printf '%s\n' "$EXPORT_OUT" | { grep -vc '^EXPORT_PATH=' || true; })"
[[ "$EXTRA_LINES" == "0" ]] || fail "export stdout not exactly one line"

grep -c . "$CUT/manifest.jsonl" | grep -qx 1 || fail "cut should contain exactly the 1 reviewed clip"
grep -q "alpha one edited" "$CUT/manifest.jsonl" || fail "cut missing certified text"
[[ -f "$CUT/cut.json" ]] || fail "cut.json missing"
ls "$CUT"/audio/* > /dev/null 2>&1 || fail "copy-audio did not copy audio"

WANT_SHA="$(python3 -c "import json,sys;print(json.load(open('$CUT/cut.json'))['manifest_sha256'])")"
GOT_SHA="$(shasum -a 256 "$CUT/manifest.jsonl" | cut -d' ' -f1)"
[[ "$WANT_SHA" == "$GOT_SHA" ]] || fail "cut.json sha mismatch"

echo "review-e2e: ALL PASS ($BIN)"
