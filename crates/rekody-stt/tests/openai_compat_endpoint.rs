//! The custom endpoint's request path, against a real socket.
//!
//! Everything else about `OpenAiCompatEngine` is unit tested, but the part
//! that matters most to a user pointing Rekody at their own server is the
//! round trip: does it send a request that server understands, and does a
//! server that answers with something else produce a message they can act on?
//!
//! A tiny loopback HTTP server answers a canned response. No daemon, no
//! microphone, no network beyond 127.0.0.1, so this is safe to run anywhere.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;

use rekody_stt::{OpenAiCompatEngine, SttEngine};

/// Serve exactly one request, then return what the client sent.
///
/// Reads headers, honours Content-Length so the multipart body is drained
/// (a client that gets its connection closed mid-upload sees a transport
/// error rather than the response we want to test).
fn serve_once(
    status_line: &'static str,
    content_type: &'static str,
    body: &'static str,
) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));

        let mut request = String::new();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).expect("read header") == 0 {
                break;
            }
            if let Some(value) = line
                .to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .and_then(|v| v.parse::<usize>().ok())
            {
                content_length = value;
            }
            let done = line == "\r\n" || line == "\n";
            request.push_str(&line);
            if done {
                break;
            }
        }

        let mut body_bytes = vec![0u8; content_length];
        reader.read_exact(&mut body_bytes).expect("read body");
        request.push_str(&String::from_utf8_lossy(&body_bytes));

        let response = format!(
            "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).expect("write");
        stream.flush().ok();
        request
    });
    (format!("http://{addr}/v1"), handle)
}

/// One second of quiet audio: enough to make a real request without the
/// empty-input short circuit.
fn samples() -> Vec<f32> {
    vec![0.0f32; 16_000]
}

/// The happy path a self-hosted server sees, plus the shape of what Rekody
/// actually sends it.
#[tokio::test]
async fn transcribes_against_an_openai_compatible_server() {
    let (base_url, server) = serve_once(
        "HTTP/1.1 200 OK",
        "application/json",
        "{\"text\":\"  hello there  \"}",
    );
    let engine = OpenAiCompatEngine::custom(
        &base_url,
        "whisper-1".into(),
        "sk-test".into(),
        Some("en".into()),
    )
    .expect("loopback http is allowed");

    let transcript = engine.transcribe(&samples()).await.expect("transcription");
    assert_eq!(transcript.text, "hello there", "the text is trimmed");

    let request = server.join().expect("server thread");
    assert!(
        request.starts_with("POST /v1/audio/transcriptions "),
        "wrong path: {}",
        &request[..40.min(request.len())]
    );
    // Header names arrive lowercased over the wire.
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-test")
    );
    assert!(request.contains("multipart/form-data"));
    assert!(request.contains("name=\"model\""));
    assert!(request.contains("whisper-1"));
    assert!(request.contains("name=\"language\""));
    assert!(request.contains("name=\"file\""));
    assert!(request.contains("RIFF"), "the WAV payload is uploaded");
}

/// A server you run yourself usually wants no key at all, and an empty
/// bearer token makes some of them reject the request outright.
#[tokio::test]
async fn an_empty_key_sends_no_authorization_header() {
    let (base_url, server) = serve_once("HTTP/1.1 200 OK", "application/json", "{\"text\":\"ok\"}");
    let engine = OpenAiCompatEngine::custom(&base_url, "whisper-1".into(), String::new(), None)
        .expect("no key is allowed");

    engine.transcribe(&samples()).await.expect("transcription");

    let request = server.join().expect("server thread");
    assert!(
        !request.to_ascii_lowercase().contains("authorization:"),
        "sent an auth header with no key"
    );
    // No language field either, so the endpoint auto-detects.
    assert!(!request.contains("name=\"language\""));
}

/// The failure this whole guard exists for: someone points Rekody at a URL
/// that answers, but is not a transcription API. The message has to say so,
/// not surface a parse error.
#[tokio::test]
async fn a_non_transcription_endpoint_fails_in_plain_language() {
    let (base_url, server) = serve_once(
        "HTTP/1.1 200 OK",
        "text/html",
        "<!doctype html><title>Sign in</title>",
    );
    let engine = OpenAiCompatEngine::custom(&base_url, "whisper-1".into(), String::new(), None)
        .expect("loopback http is allowed");

    let err = engine
        .transcribe(&samples())
        .await
        .expect_err("html is not a transcript");
    let message = err.to_string();
    assert!(
        message.contains("not with a transcription"),
        "unhelpful: {message}"
    );
    assert!(
        message.contains("\"text\" field"),
        "must say what was expected: {message}"
    );
    assert!(
        message.contains("Sign in"),
        "must quote what came back: {message}"
    );
    assert!(
        !message.contains("expected value"),
        "leaked a serde parse error: {message}"
    );
    let _ = server.join();
}

/// An error status quotes the body rather than swallowing it, and names the
/// host so a misconfigured endpoint is identifiable.
#[tokio::test]
async fn an_error_status_reports_the_body() {
    let (base_url, server) = serve_once(
        "HTTP/1.1 401 Unauthorized",
        "application/json",
        "{\"error\":\"invalid api key\"}",
    );
    let engine = OpenAiCompatEngine::custom(&base_url, "whisper-1".into(), "sk-bad".into(), None)
        .expect("loopback http is allowed");

    let err = engine
        .transcribe(&samples())
        .await
        .expect_err("401 is an error");
    let message = err.to_string();
    assert!(message.contains("401"), "must carry the status: {message}");
    assert!(
        message.contains("invalid api key"),
        "must quote the body: {message}"
    );
    assert!(
        message.contains("127.0.0.1"),
        "must name the destination: {message}"
    );
    assert!(
        !message.contains("sk-bad"),
        "the key must never reach an error message"
    );
    let _ = server.join();
}
