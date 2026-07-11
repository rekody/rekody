//! The localhost HTTP server: review page, clip JSON, audio files, and the
//! decision endpoint. Binds 127.0.0.1 only; there is no remote surface.
//!
//! tiny_http keeps this synchronous and single-user simple: one accept loop,
//! one request at a time, no async stack. Plenty for a personal review tool.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};
use tiny_http::{Header, Method, Response};

use crate::store::{self, SharedStore};

/// The whole review UI, embedded so the binary is self-contained.
const PAGE: &str = include_str!("page.html");

type Resp = Response<std::io::Cursor<Vec<u8>>>;

/// Serve forever on 127.0.0.1:`port`.
pub fn serve(shared: SharedStore, audio_dir: PathBuf, port: u16) -> Result<()> {
    let server = tiny_http::Server::http(("127.0.0.1", port))
        .map_err(|e| anyhow::anyhow!("binding 127.0.0.1:{port}: {e}"))?;
    tracing::info!("review server up on 127.0.0.1:{port} · open http://localhost:{port}");
    for mut request in server.incoming_requests() {
        let response = route(&shared, &audio_dir, &mut request);
        if let Err(e) = request.respond(response) {
            tracing::debug!("client hung up mid-response: {e}");
        }
    }
    Ok(())
}

fn route(shared: &SharedStore, audio_dir: &Path, request: &mut tiny_http::Request) -> Resp {
    // Strip any query string; every route here is path-only.
    let path = request.url().split('?').next().unwrap_or("/").to_string();
    let method = request.method().clone();
    match (method, path.as_str()) {
        (Method::Get, "/") => html(PAGE),
        (Method::Get, "/api/clips") => json_status(200, &store::lock(shared).clips_json()),
        (Method::Get, p) if p.starts_with("/audio/") => {
            let range = range_header(request);
            serve_audio(audio_dir, &p["/audio/".len()..], range.as_deref())
        }
        (Method::Post, "/api/decision") => post_decision(shared, request),
        _ => json_status(404, &json!({"error": "not found"})),
    }
}

/// Handle POST /api/decision: `{audio_filepath, decision, final_text?}`.
fn post_decision(shared: &SharedStore, request: &mut tiny_http::Request) -> Resp {
    let mut body = String::new();
    if let Err(e) = std::io::Read::read_to_string(request.as_reader(), &mut body) {
        return json_status(400, &json!({"error": format!("reading body: {e}")}));
    }
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return json_status(400, &json!({"error": format!("invalid JSON: {e}")})),
    };
    let (Some(path), Some(decision)) = (v["audio_filepath"].as_str(), v["decision"].as_str())
    else {
        return json_status(
            400,
            &json!({"error": "audio_filepath and decision are required"}),
        );
    };
    match store::lock(shared).apply_decision(path, decision, v["final_text"].as_str()) {
        Ok(clip) => json_status(200, &clip),
        Err(e) => json_status(400, &json!({"error": format!("{e:#}")})),
    }
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
            ));
    }
    Response::from_data(data)
        .with_header(header("Content-Type", mime))
        .with_header(header("Accept-Ranges", "bytes"))
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
}

fn json_status(code: u16, v: &Value) -> Resp {
    Response::from_string(v.to_string())
        .with_status_code(code)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
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
        assert!(PAGE.contains("<title>Rekody · Label review</title>"));
    }
}
