#!/usr/bin/env bash
# review-e2e.sh — sandboxed end-to-end of the review product against a
# SYNTHETIC dataset: server contracts, a decision, the inbox counter, the
# hours-reviewed line, an export cut, a zip download, and a zip import into
# a second fresh dataset, exercised through a real rekody binary exactly as
# a user would. No personal data, no daemon, no hotkeys — safe anywhere, CI
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

# ── Synthetic dataset: 1 s tone WAVs + manifest rows ─────────────────────────
# (.wav paths: the store is extension-agnostic and this keeps the script
# independent of afconvert.) Parameterized by dataset dir and clip names, so
# the import half can build a second, independent machine the same way.
seed_dataset() {
  local dir="$1"; shift
  mkdir -p "$dir/audio"
  python3 - "$dir" "$@" <<'SEED'
import json, math, struct, sys, wave
ds, names = sys.argv[1], sys.argv[2:]
clips = {
    "a": ("alpha one", "2026-07-01T10-00-00"),
    "b": ("bravo two", "2026-07-02T10-00-00"),
    "c": ("charlie three", "2026-07-03T10-00-00"),
    "d": ("delta four", "2026-07-04T10-00-00"),
}
with open(f"{ds}/manifest.jsonl", "w") as f:
    for name in names:
        text, stamp = clips[name]
        with wave.open(f"{ds}/audio/{name}.wav", "wb") as w:
            w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000)
            w.writeframes(b"".join(
                struct.pack("<h", int(8000 * math.sin(2 * math.pi * 440 * t / 16000)))
                for t in range(16000)))
        f.write(json.dumps({"audio_filepath": f"audio/{name}.wav", "text": text,
                            "duration": 1.0, "engine": "nemotron",
                            "timestamp": stamp}) + "\n")
SEED
}

seed_dataset "$DS" a b c

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

# ── Hours reviewed: the page's headline number ───────────────────────────────
# One reviewed 1 s clip: the seconds are exact and the phrase comes from the
# server, so the page and the CLI can never disagree about the wording.
STATE="$(curl -s "$URL/api/state")"
python3 - "$STATE" <<'HOURS'
import json, sys
p = json.loads(sys.argv[1])["progress"]
assert abs(p["reviewed_duration_secs"] - 1.0) < 1e-6, p
assert p["reviewed_label"] == "0 minutes", p
HOURS
curl -s "$URL/" | grep -q 'minutes reviewed' || fail "page missing the hours-reviewed line"

# ── Zip download, the way the page asks for it ───────────────────────────────
curl -s -D "$TMP/zip-headers.txt" -o "$TMP/download.zip" "$URL/api/export.zip"
tr -d '\r' < "$TMP/zip-headers.txt" | grep -qi 'content-disposition: attachment; filename="cut-.*\.zip"' \
  || { cat "$TMP/zip-headers.txt" >&2; fail "/api/export.zip is not an attachment named cut-<date>-<hash>.zip"; }
python3 - "$TMP/download.zip" <<'ZIPCHK'
import sys, zipfile
names = zipfile.ZipFile(sys.argv[1]).namelist()
assert any(n.endswith("manifest.jsonl") for n in names), names
assert any(n.endswith("cut.json") for n in names), names
assert any("/audio/" in n for n in names), names
ZIPCHK
[[ $? -eq 0 ]] || fail "the downloaded zip is not a self-contained cut"
# Downloading leaves nothing behind in the dataset.
[[ ! -d "$DS/exports" ]] || fail "the zip download littered the dataset with a cut folder"

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

# ── Headless zip export ──────────────────────────────────────────────────────
ZIP_OUT="$(REKODY_TRAINING_DIR="$DS" "$BIN" review export --zip 2> "$TMP/zip-err.log" || true)"
ZIP="$(printf '%s\n' "$ZIP_OUT" | grep -m1 '^EXPORT_PATH=' | cut -d= -f2- || true)"
[[ -n "$ZIP" && -f "$ZIP" ]] || { cat "$TMP/zip-err.log" >&2; fail "EXPORT_PATH did not point at a zip: '$ZIP_OUT'"; }
[[ "$ZIP" == *.zip ]] || fail "--zip produced '$ZIP', not a .zip"
EXTRA_LINES="$(printf '%s\n' "$ZIP_OUT" | { grep -vc '^EXPORT_PATH=' || true; })"
[[ "$EXTRA_LINES" == "0" ]] || fail "export --zip stdout not exactly one line"

# ── Import that zip into a SECOND, fresh synthetic dataset ───────────────────
# Machine B holds clip a still unreviewed (so the correction has to land) and
# clip d, which machine A has never seen (so it must survive untouched).
DS2="$TMP/dataset-b"
seed_dataset "$DS2" a d
grep -q "alpha one edited" "$DS2/manifest.jsonl" && fail "machine B started out already corrected"

IMPORT_OUT="$(REKODY_TRAINING_DIR="$DS2" "$BIN" review import "$ZIP" 2> "$TMP/import-err.log" || true)"
SUMMARY="$(printf '%s\n' "$IMPORT_OUT" | grep -m1 '^IMPORT_SUMMARY=' | cut -d= -f2- || true)"
[[ -n "$SUMMARY" ]] || { cat "$TMP/import-err.log" >&2; fail "IMPORT_SUMMARY contract broken: '$IMPORT_OUT'"; }
EXTRA_LINES="$(printf '%s\n' "$IMPORT_OUT" | { grep -vc '^IMPORT_SUMMARY=' || true; })"
[[ "$EXTRA_LINES" == "0" ]] || fail "import stdout not exactly one line"

python3 - "$SUMMARY" <<'MERGED'
import json, sys
s = json.loads(sys.argv[1])
assert s["imported"] == 1, s
assert s["new_clips"] == 0, s
assert s["updated_clips"] == 1, s
assert s["audio_copied"] == 0, s
assert s["reviewed_clips"] == 1, s
MERGED

# The corrected text arrived, the audio is intact, and machine B's own clip
# was left exactly as it was.
grep -q "alpha one edited" "$DS2/manifest.jsonl" || fail "the correction did not merge into machine B"
grep -q '"merged_from"' "$DS2/decisions.jsonl" || fail "merged decisions carry no provenance"
[[ -f "$DS2/audio/a.wav" ]] || fail "machine B lost its audio"
cmp -s "$DS/audio/a.wav" "$DS2/audio/a.wav" || fail "machine B's audio does not match the source clip"
grep -q "delta four" "$DS2/manifest.jsonl" || fail "the import dropped a clip machine B already had"
grep -c . "$DS2/manifest.jsonl" | grep -qx 2 || fail "machine B's manifest row count changed"
[[ -f "$DS2/manifest.jsonl.bak-import" && -f "$DS2/decisions.jsonl.bak-import" ]] \
  || fail "the import did not back up before writing"

# ── A second import of the same zip changes nothing ──────────────────────────
BEFORE_M="$(shasum -a 256 "$DS2/manifest.jsonl" | cut -d' ' -f1)"
BEFORE_D="$(shasum -a 256 "$DS2/decisions.jsonl" | cut -d' ' -f1)"
SUMMARY2="$(REKODY_TRAINING_DIR="$DS2" "$BIN" review import "$ZIP" 2>/dev/null | \
  grep -m1 '^IMPORT_SUMMARY=' | cut -d= -f2-)"
python3 - "$SUMMARY2" <<'AGAIN'
import json, sys
s = json.loads(sys.argv[1])
assert s["imported"] == 0, s
assert s["audio_copied"] == 0, s
assert s["skipped_conflicts"] == 1, s
AGAIN
[[ "$BEFORE_M" == "$(shasum -a 256 "$DS2/manifest.jsonl" | cut -d' ' -f1)" ]] \
  || fail "a repeat import rewrote the manifest"
[[ "$BEFORE_D" == "$(shasum -a 256 "$DS2/decisions.jsonl" | cut -d' ' -f1)" ]] \
  || fail "a repeat import appended to the decision log"

# ── The same import, through the endpoint the page posts to ──────────────────
# The page sends the picked file as the whole request body, which is exactly
# what --data-binary does. A third fresh dataset keeps this independent of
# the CLI run above.
DS3="$TMP/dataset-c"
seed_dataset "$DS3" a d
"$BIN" review --dir "$DS3" --port "$((PORT + 1))" --no-open --auto-exit-secs 0 \
  > "$TMP/stdout3.log" 2> "$TMP/stderr3.log" &
SRV_PID=$!
URL3=""
for _ in $(seq 1 50); do
  URL3="$(grep -m1 '^REVIEW_URL=' "$TMP/stdout3.log" 2>/dev/null | cut -d= -f2- || true)"
  [[ -n "$URL3" ]] && break
  kill -0 "$SRV_PID" 2>/dev/null || { cat "$TMP/stderr3.log" >&2; fail "second server died on startup"; }
  sleep 0.2
done
[[ -n "$URL3" ]] || fail "second server never printed REVIEW_URL"

UPLOADED="$(curl -s -X POST --data-binary "@$ZIP" "$URL3/api/import")"
python3 - "$UPLOADED" <<'UPLOAD'
import json, sys
s = json.loads(sys.argv[1])
assert "error" not in s, s
assert s["imported"] == 1, s
assert s["reviewed_clips"] == 1, s
assert s["reviewed_label"] == "0 minutes", s
UPLOAD
grep -q "alpha one edited" "$DS3/manifest.jsonl" || fail "the upload endpoint did not merge the correction"

kill "$SRV_PID"; wait "$SRV_PID" 2>/dev/null || true; SRV_PID=""

echo "review-e2e: ALL PASS ($BIN)"
