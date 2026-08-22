//! The localhost HTTP server: review page, state and clip JSON, audio
//! files, the start/decide/export/import endpoints, and the ping probe.
//! Binds 127.0.0.1 by default; the opt-in `--lan` flag binds 0.0.0.0 so a
//! phone on the same Wi-Fi can open the page.
//!
//! tiny_http keeps this synchronous and single-user simple: one accept loop,
//! one request at a time, no async stack. Plenty for a personal review tool.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};
use tiny_http::{Header, Method, Response};

use crate::store::{self, Mode, SharedStore};
use crate::{SharedRun, spawn_teacher_once};

/// The whole review UI, embedded so the binary is self-contained.
const PAGE: &str = include_str!("page.html");

/// Ports tried after the preferred one: 7878 busy means 7879..7887 are the
/// candidates before giving up.
const PORT_FALLBACK_TRIES: u16 = 9;

/// Response header on /api/ping that identifies a live review server.
/// Locked contract: the Mac app probes it to avoid a double launch.
const PING_HEADER: (&str, &str) = ("X-Rekody-Review", "1");

/// Every route answers with a boxed response, so a small JSON body and a
/// multi-hundred-megabyte cut streamed straight off a temp file can come
/// back through the same signature.
type Resp = tiny_http::ResponseBox;

/// Bind on `preferred`, walking up through the next `PORT_FALLBACK_TRIES`
/// ports when busy. Returns the server and the port it actually landed on.
/// The host is 127.0.0.1 unless `lan` widens it to 0.0.0.0.
pub(crate) fn bind(preferred: u16, lan: bool) -> Result<(tiny_http::Server, u16)> {
    let host = if lan { "0.0.0.0" } else { "127.0.0.1" };
    let mut last_err = String::new();
    for offset in 0..=PORT_FALLBACK_TRIES {
        let Some(port) = preferred.checked_add(offset) else {
            break;
        };
        match tiny_http::Server::http((host, port)) {
            Ok(server) => {
                if offset > 0 {
                    tracing::info!("port {preferred} was busy; using {port} instead");
                }
                return Ok((server, port));
            }
            Err(e) => last_err = format!("binding {host}:{port}: {e}"),
        }
    }
    anyhow::bail!(
        "no free port between {preferred} and {}: {last_err}",
        preferred.saturating_add(PORT_FALLBACK_TRIES)
    )
}

/// The --auto-exit-secs rule: fire once the page has been silent for the
/// whole window. 0 disables auto-exit entirely.
pub(crate) fn should_auto_exit(auto_exit_secs: u64, idle: Duration) -> bool {
    auto_exit_secs > 0 && idle >= Duration::from_secs(auto_exit_secs)
}

/// Accept loop. Every request (the page pings /api/ping every 30 s while a
/// tab is open) resets the idle clock; when auto-exit is armed and the
/// clock runs out, returns Ok so the process ends with exit code 0. A
/// teacher pass still running at that point is safe to drop: it is
/// resumable by design and picks up where it stopped on the next launch.
pub(crate) fn run(
    server: tiny_http::Server,
    shared: SharedStore,
    run_state: SharedRun,
    root: PathBuf,
    auto_exit_secs: u64,
) -> Result<()> {
    let audio_dir = root.join("audio");
    let mut last_request = Instant::now();
    loop {
        match server.recv_timeout(Duration::from_millis(500)) {
            Ok(Some(mut request)) => {
                last_request = Instant::now();
                let response = route(&shared, &run_state, &root, &audio_dir, &mut request);
                if let Err(e) = request.respond(response) {
                    tracing::debug!("client hung up mid-response: {e}");
                }
            }
            Ok(None) => {
                if should_auto_exit(auto_exit_secs, last_request.elapsed()) {
                    tracing::info!("no page activity for {auto_exit_secs}s; exiting");
                    return Ok(());
                }
            }
            Err(e) => return Err(anyhow::anyhow!("accepting connections: {e}")),
        }
    }
}

fn route(
    shared: &SharedStore,
    run_state: &SharedRun,
    root: &Path,
    audio_dir: &Path,
    request: &mut tiny_http::Request,
) -> Resp {
    let url = request.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (url, None),
    };
    let query = query.as_deref();
    let method = request.method().clone();
    match (method, path.as_str()) {
        (Method::Get, "/") => html(PAGE),
        (Method::Get, "/api/ping") => {
            json_status(200, &json!({"ok": true})).with_header(header(PING_HEADER.0, PING_HEADER.1))
        }
        (Method::Get, "/api/state") => json_status(200, &state_json(shared, run_state)),
        (Method::Get, "/api/clips") => json_status(200, &store::lock(shared).clips_json()),
        (Method::Get, "/api/export") => export_manifest(root),
        (Method::Get, "/api/export.zip") => export_zip(shared, query),
        (Method::Get, p) if p.starts_with("/audio/") => {
            let range = range_header(request);
            serve_audio(audio_dir, &p["/audio/".len()..], range.as_deref())
        }
        (Method::Post, "/api/export") => post_export(shared, request),
        (Method::Post, "/api/import") => post_import(shared, request),
        (Method::Post, "/api/start") => post_start(shared, run_state, root, request),
        // /api/decision is the pre-product name for the same endpoint; kept
        // so an already-open tab or script keeps working across an update.
        (Method::Post, "/api/decide") | (Method::Post, "/api/decision") => {
            post_decide(shared, request)
        }
        _ => json_status(404, &json!({"error": "not found"})),
    }
}

// ---------------------------------------------------------------------------
// State + session endpoints
// ---------------------------------------------------------------------------

/// GET /api/state: which screen the page shows and every number on it.
fn state_json(shared: &SharedStore, run_state: &SharedRun) -> Value {
    let s = store::lock(shared);
    let (mode_str, opinion_enabled) = match s.effective_mode() {
        Mode::FirstRun => ("first_run", false),
        Mode::Session { second_opinion } => ("session", second_opinion),
    };
    let reviewed_secs = s.reviewed_duration_secs();
    let scoring = if opinion_enabled {
        json!({ "done": s.teacher_count(), "total": s.clip_count() })
    } else {
        Value::Null
    };
    let mut out = json!({
        "mode": mode_str,
        "dataset": {
            "path": s.root().display().to_string(),
            "found": s.manifest_found(),
            "clips": s.clip_count(),
            "hours": s.total_duration_secs() / 3600.0,
        },
        "second_opinion": {
            "enabled": opinion_enabled,
            "model_present": crate::teacher::model_present(),
            "scoring": scoring,
        },
        "progress": {
            "reviewed": s.reviewed_count(),
            "total": s.clip_count() + s.deleted_count(),
            // Hours of audio the user has actually reviewed, and the plain
            // words for it. The page shows this as the headline number, so
            // the phrasing is settled here once (minutes below an hour, one
            // decimal above) rather than in two places.
            "reviewed_duration_secs": reviewed_secs,
            "reviewed_label": store::fmt_reviewed_duration(reviewed_secs),
        },
    });
    drop(s);
    let r = run_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((received, total)) = r.model_download {
        out["model_download"] = json!({ "received": received, "total": total });
    }
    out
}

/// POST /api/start `{second_opinion: bool}`: record how the session runs
/// and kick off the matching work. Idempotent, and an active second opinion
/// is never downgraded, so a stray originals-only POST cannot stop scoring
/// that is already underway.
fn post_start(
    shared: &SharedStore,
    run_state: &SharedRun,
    root: &Path,
    request: &mut tiny_http::Request,
) -> Resp {
    let v = match read_json_body(request) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(want) = v["second_opinion"].as_bool() else {
        return json_status(400, &json!({"error": "second_opinion (bool) is required"}));
    };
    let effective;
    {
        let mut s = store::lock(shared);
        if !s.manifest_found() {
            return json_status(
                400,
                &json!({"error": format!("no dataset at {}", s.root().display())}),
            );
        }
        effective = want
            || matches!(
                s.effective_mode(),
                Mode::Session {
                    second_opinion: true
                }
            );
        if let Err(e) = store::write_review_state(root, effective) {
            return json_status(500, &json!({"error": format!("{e:#}")}));
        }
        if !effective && let Err(e) = s.seed_null_reviews() {
            return json_status(500, &json!({"error": format!("{e:#}")}));
        }
    }
    if effective {
        spawn_teacher_once(shared, root, run_state);
    }
    json_status(200, &state_json(shared, run_state))
}

/// POST /api/decide `{clip, action, final_text?}` (also accepts the older
/// audio_filepath/decision key names).
fn post_decide(shared: &SharedStore, request: &mut tiny_http::Request) -> Resp {
    let v = match read_json_body(request) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some((clip, action)) = decide_params(&v) else {
        return json_status(400, &json!({"error": "clip and action are required"}));
    };
    match store::lock(shared).apply_decision(clip, action, v["final_text"].as_str()) {
        Ok(clip) => json_status(200, &clip),
        Err(e) => json_status(400, &json!({"error": format!("{e:#}")})),
    }
}

/// Accept both key spellings: `{clip, action}` from the current page,
/// `{audio_filepath, decision}` from the pre-product tool.
fn decide_params(v: &Value) -> Option<(&str, &str)> {
    let clip = v["clip"]
        .as_str()
        .or_else(|| v["audio_filepath"].as_str())?;
    let action = v["action"].as_str().or_else(|| v["decision"].as_str())?;
    Some((clip, action))
}

/// POST /api/export `{until?, copy_audio?}`: write a reviewed-only cut
/// (see store::Store::export_cut) and answer with where it landed. The
/// body is optional; no body means "everything, manifest only".
fn post_export(shared: &SharedStore, request: &mut tiny_http::Request) -> Resp {
    let mut body = String::new();
    if let Err(e) = std::io::Read::read_to_string(request.as_reader(), &mut body) {
        return json_status(400, &json!({"error": format!("reading body: {e}")}));
    }
    let v: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => return json_status(400, &json!({"error": format!("invalid JSON: {e}")})),
        }
    };
    let until = match &v["until"] {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        _ => return json_status(400, &json!({"error": "until must be a YYYY-MM-DD string"})),
    };
    let opts = store::CutOptions {
        until,
        copy_audio: v["copy_audio"].as_bool().unwrap_or(false),
        out_dir: None,
    };
    match store::lock(shared).export_cut(&opts) {
        Ok(cut) => json_status(
            200,
            &json!({
                "path": cut.dir.display().to_string(),
                "until": cut.until,
                "clip_count": cut.clip_count,
                "total_duration_secs": cut.total_duration_secs,
                "manifest_sha256": cut.manifest_sha256,
            }),
        ),
        Err(e) => json_status(400, &json!({"error": format!("{e:#}")})),
    }
}

/// GET /api/export: the current manifest as a download. The manifest on
/// disk is already the reviewed truth (decisions rewrite it in place), so
/// this is a straight copy with an attachment disposition. Kept alongside
/// the POST cut endpoint so an already-open tab or script keeps working.
fn export_manifest(root: &Path) -> Resp {
    match std::fs::read(root.join("manifest.jsonl")) {
        Ok(data) => Response::from_data(data)
            .with_header(header("Content-Type", "application/x-ndjson"))
            .with_header(header(
                "Content-Disposition",
                "attachment; filename=\"manifest.jsonl\"",
            ))
            .with_header(header("Cache-Control", "no-store"))
            .boxed(),
        Err(e) => json_status(
            404,
            &json!({"error": format!("no manifest to export: {e}")}),
        ),
    }
}

/// GET /api/export.zip[?until=YYYY-MM-DD]: build a self-contained cut
/// (reviewed clips only, audio copied in) and hand it straight back as one
/// `cut-<date>-<hash8>.zip` the browser saves to Downloads. This is the
/// artifact a user mails themselves or drops on a drive.
///
/// The cut is staged in a temp folder and zipped into an unlinked temp
/// file, so a download leaves nothing behind in the dataset and the bytes
/// stream out of the file rather than through memory. Nothing is uploaded
/// anywhere: the response goes to the browser that asked for it and stops.
fn export_zip(shared: &SharedStore, query: Option<&str>) -> Resp {
    let until = query.and_then(|q| query_param(q, "until"));
    let staging = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            return json_status(500, &json!({"error": format!("making a temp folder: {e}")}));
        }
    };
    let cut = match store::lock(shared).export_cut(&store::CutOptions {
        until,
        copy_audio: true,
        out_dir: Some(staging.path().to_path_buf()),
    }) {
        Ok(cut) => cut,
        Err(e) => return json_status(400, &json!({"error": format!("{e:#}")})),
    };
    let name = cut
        .dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("cut")
        .to_string();

    let mut file = match tempfile::tempfile() {
        Ok(f) => f,
        Err(e) => return json_status(500, &json!({"error": format!("making a temp file: {e}")})),
    };
    if let Err(e) = crate::cut::write_zip(&cut.dir, &mut file) {
        return json_status(500, &json!({"error": format!("{e:#}")}));
    }
    if let Err(e) = std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(0)) {
        return json_status(500, &json!({"error": format!("rewinding the zip: {e}")}));
    }
    tracing::info!(clips = cut.clip_count, cut = %name, "sending a cut to the browser");
    Response::from_file(file)
        .with_header(header("Content-Type", "application/zip"))
        .with_header(header(
            "Content-Disposition",
            &format!("attachment; filename=\"{name}.zip\""),
        ))
        .with_header(header("Cache-Control", "no-store"))
        .boxed()
}

/// POST /api/import: the request body is the cut zip itself (the page
/// posts the picked file straight through, no multipart), merged into this
/// dataset by `store::Store::import_cut`. Answers with the summary.
fn post_import(shared: &SharedStore, request: &mut tiny_http::Request) -> Resp {
    let mut upload = match tempfile::Builder::new().suffix(".zip").tempfile() {
        Ok(f) => f,
        Err(e) => return json_status(500, &json!({"error": format!("making a temp file: {e}")})),
    };
    let mut body = std::io::Read::take(request.as_reader(), crate::cut::MAX_UNPACKED_BYTES);
    let size = match std::io::copy(&mut body, upload.as_file_mut()) {
        Ok(n) => n,
        Err(e) => return json_status(400, &json!({"error": format!("reading the upload: {e}")})),
    };
    if size == 0 {
        return json_status(400, &json!({"error": "no cut in that upload"}));
    }
    let opened = match crate::cut::open(upload.path()) {
        Ok(c) => c,
        Err(e) => return json_status(400, &json!({"error": format!("{e:#}")})),
    };
    match store::lock(shared).import_cut(&opened.root, &opened.label) {
        Ok(summary) => {
            tracing::info!(
                imported = summary.imported,
                skipped = summary.skipped(),
                audio = summary.audio_copied,
                source = %summary.source,
                "cut merged"
            );
            json_status(200, &summary.to_json())
        }
        Err(e) => json_status(400, &json!({"error": format!("{e:#}")})),
    }
}

/// One value out of a query string. Only `+` and `%XX` in the ASCII range
/// are decoded, which covers everything the page actually sends.
fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
        .filter(|v| !v.is_empty())
}

/// Minimal percent-decoding for query values.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read and parse a JSON request body, or produce the 400 to send back.
fn read_json_body(request: &mut tiny_http::Request) -> Result<Value, Box<Resp>> {
    let mut body = String::new();
    if let Err(e) = std::io::Read::read_to_string(request.as_reader(), &mut body) {
        return Err(Box::new(json_status(
            400,
            &json!({"error": format!("reading body: {e}")}),
        )));
    }
    serde_json::from_str(&body).map_err(|e| {
        Box::new(json_status(
            400,
            &json!({"error": format!("invalid JSON: {e}")}),
        ))
    })
}

// ---------------------------------------------------------------------------
// Audio serving
// ---------------------------------------------------------------------------

/// Resolve a requested audio filename inside `audio_dir`, or `None` for
/// anything that smells like traversal.
///
/// Two layers: a strict character allowlist first (capture filenames are
/// timestamps plus hex suffixes, so path syntax of any kind is rejected
/// before the filesystem is touched), then a canonicalize-and-prefix check
/// so even a crafted name that slipped through could not resolve outside
/// the audio directory.
fn safe_audio_path(audio_dir: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.starts_with('.') || name.contains("..") {
        return None;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return None;
    }
    if !(name.ends_with(".flac") || name.ends_with(".wav")) {
        return None;
    }
    let resolved = audio_dir.join(name).canonicalize().ok()?;
    let base = audio_dir.canonicalize().ok()?;
    if !resolved.starts_with(&base) {
        return None;
    }
    Some(resolved)
}

fn serve_audio(audio_dir: &Path, name: &str, range: Option<&str>) -> Resp {
    let Some(path) = safe_audio_path(audio_dir, name) else {
        return json_status(404, &json!({"error": "no such clip"}));
    };
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => return json_status(500, &json!({"error": format!("reading clip: {e}")})),
    };
    let mime = if name.ends_with(".wav") {
        "audio/wav"
    } else {
        "audio/flac"
    };
    let len = data.len() as u64;
    // Minimal single-range support: Safari probes media with Range requests
    // and expects 206s back before it will play.
    if let Some((start, end)) = range.and_then(|r| parse_range(r, len)) {
        let slice = data[start as usize..=end as usize].to_vec();
        return Response::from_data(slice)
            .with_status_code(206)
            .with_header(header("Content-Type", mime))
            .with_header(header("Accept-Ranges", "bytes"))
            .with_header(header(
                "Content-Range",
                &format!("bytes {start}-{end}/{len}"),
            ))
            .boxed();
    }
    Response::from_data(data)
        .with_header(header("Content-Type", mime))
        .with_header(header("Accept-Ranges", "bytes"))
        .boxed()
}

/// Parse a single-range `bytes=` header against a body of `len` bytes.
/// Anything unusual (multi-range, malformed, out of bounds) returns `None`
/// and the caller serves the whole file with a plain 200.
fn parse_range(value: &str, len: u64) -> Option<(u64, u64)> {
    if len == 0 || value.contains(',') {
        return None;
    }
    let spec = value.trim().strip_prefix("bytes=")?;
    let (start_s, end_s) = spec.split_once('-')?;
    let (start, end) = match (start_s.is_empty(), end_s.is_empty()) {
        // bytes=a-b
        (false, false) => (
            start_s.parse::<u64>().ok()?,
            end_s.parse::<u64>().ok()?.min(len - 1),
        ),
        // bytes=a-  (from a to the end)
        (false, true) => (start_s.parse::<u64>().ok()?, len - 1),
        // bytes=-n  (final n bytes)
        (true, false) => {
            let n = end_s.parse::<u64>().ok()?;
            if n == 0 {
                return None;
            }
            (len.saturating_sub(n), len - 1)
        }
        (true, true) => return None,
    };
    if start > end || start >= len {
        return None;
    }
    Some((start, end))
}

fn range_header(request: &tiny_http::Request) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .map(|h| h.value.as_str().to_string())
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

fn header(field: &str, value: &str) -> Header {
    Header::from_bytes(field.as_bytes(), value.as_bytes()).expect("static header is valid")
}

fn html(page: &str) -> Resp {
    Response::from_string(page)
        .with_header(header("Content-Type", "text/html; charset=utf-8"))
        .with_header(header("Cache-Control", "no-store"))
        .boxed()
}

fn json_status(code: u16, v: &Value) -> Resp {
    Response::from_string(v.to_string())
        .with_status_code(code)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_path_guard_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio");
        std::fs::create_dir_all(&audio).unwrap();
        std::fs::write(audio.join("clip.flac"), b"x").unwrap();
        // A juicy target one level up.
        std::fs::write(dir.path().join("manifest.jsonl"), b"secret").unwrap();

        assert!(safe_audio_path(&audio, "clip.flac").is_some());

        for bad in [
            "../manifest.jsonl",
            "..%2Fmanifest.jsonl",
            "audio/../../manifest.jsonl",
            "sub/clip.flac",
            "/etc/passwd",
            ".hidden.flac",
            "clip.mp3",
            "clip.flac ",
            "",
            "..",
        ] {
            assert!(
                safe_audio_path(&audio, bad).is_none(),
                "guard let through {bad:?}"
            );
        }

        // Valid shape but nonexistent file: canonicalize fails, so 404.
        assert!(safe_audio_path(&audio, "missing.flac").is_none());
    }

    #[test]
    fn range_parsing_handles_the_common_shapes() {
        assert_eq!(parse_range("bytes=0-1", 100), Some((0, 1)));
        assert_eq!(parse_range("bytes=10-", 100), Some((10, 99)));
        assert_eq!(parse_range("bytes=-20", 100), Some((80, 99)));
        // End clamps to the body.
        assert_eq!(parse_range("bytes=0-999", 100), Some((0, 99)));
        // Out of bounds, multi-range, and junk all fall back to a full 200.
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=5-2", 100), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
        assert_eq!(parse_range("bytes=-0", 100), None);
        assert_eq!(parse_range("bytes=0-1", 0), None);
    }

    #[test]
    fn page_respects_brand_copy_rules() {
        // No em dashes anywhere in UI copy, and the exact page title.
        assert!(!PAGE.contains('\u{2014}'), "page.html contains an em dash");
        assert!(PAGE.contains("<title>Rekody · Review</title>"));
        // The renamed preview label shipped with the redesign.
        assert!(PAGE.contains("WILL SAVE"));
        // The keep-alive ping the auto-exit timer relies on.
        assert!(PAGE.contains("/api/ping"));
        // Inbox counter framing: awaiting + reviewed, never a completion
        // fraction against a growing total.
        assert!(PAGE.contains("awaiting review"));
        assert!(!PAGE.contains("of 0 reviewed"));
        // The export control posts to the cut endpoint.
        assert!(PAGE.contains("ex-until") && PAGE.contains("copy_audio"));
        // Hours reviewed is the headline number, and it is edited audio
        // only: no remaining/unedited hours anywhere.
        assert!(PAGE.contains("reviewedLabel()"));
        assert!(PAGE.contains("minutes reviewed"));
        assert!(!PAGE.to_lowercase().contains("hours remaining"));
        // The finish state names what the user has and hands it over.
        assert!(PAGE.contains("Nothing left to review."));
        assert!(PAGE.contains("/api/export.zip") && PAGE.contains("/api/import"));
        // No milestone or target framing: that is still an open question.
        assert!(!PAGE.contains("of 5 hours"));
        // Nothing may suggest a managed training service exists. The only
        // forward-looking CTA is the early-access mailto.
        assert!(PAGE.contains("mailto:hi@rekody.com"));
        assert!(!PAGE.contains("We train on your dataset"));
        assert!(!PAGE.contains("Rekody GPUs"));
    }

    #[test]
    fn query_values_are_read_the_way_the_page_sends_them() {
        assert_eq!(
            query_param("until=2026-08-19", "until").as_deref(),
            Some("2026-08-19")
        );
        assert_eq!(
            query_param("a=1&until=2026-08-19&b=2", "until").as_deref(),
            Some("2026-08-19")
        );
        assert_eq!(
            query_param("until=2026%2D08%2D19", "until").as_deref(),
            Some("2026-08-19")
        );
        assert_eq!(query_param("until=", "until"), None);
        assert_eq!(query_param("other=x", "until"), None);
        assert_eq!(query_param("", "until"), None);
    }

    #[test]
    fn auto_exit_fires_only_after_the_idle_window() {
        assert!(
            !should_auto_exit(0, Duration::from_secs(86400)),
            "0 disables auto-exit"
        );
        assert!(!should_auto_exit(5, Duration::from_millis(4900)));
        assert!(should_auto_exit(5, Duration::from_secs(5)));
        assert!(should_auto_exit(1, Duration::from_millis(1000)));
    }

    #[test]
    fn bind_walks_past_a_busy_port() {
        // Occupy an ephemeral port, then ask bind() for exactly that port.
        // Never touches fixed ports, so a live review server is safe.
        let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = blocker.local_addr().unwrap().port();
        if busy > u16::MAX - PORT_FALLBACK_TRIES {
            return; // no fallback room above this port; skip the rare case
        }
        let (server, chosen) = bind(busy, false).expect("a nearby port should be free");
        assert_ne!(chosen, busy, "must not claim the busy port");
        assert!(chosen > busy && chosen <= busy + PORT_FALLBACK_TRIES);
        drop(server);
        drop(blocker);
    }

    #[test]
    fn decide_params_accepts_both_key_spellings() {
        let new = json!({"clip": "audio/a.flac", "action": "delete"});
        assert_eq!(decide_params(&new), Some(("audio/a.flac", "delete")));
        let old = json!({"audio_filepath": "audio/a.flac", "decision": "keep_original"});
        assert_eq!(decide_params(&old), Some(("audio/a.flac", "keep_original")));
        assert_eq!(decide_params(&json!({"clip": "x"})), None);
        assert_eq!(decide_params(&json!({"action": "undo"})), None);
    }
}
