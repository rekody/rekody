//! Integration test for the HUD IPC server (docs/design/hud-protocol.md):
//! bind a real unix socket, connect a client, and assert the exact NDJSON
//! protocol lines on the wire.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rekody_core::hud::{HudEvent, HudServer};

/// Short, unique socket path (unix socket paths are length-limited on macOS).
fn temp_socket_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rekody-hud-{tag}-{}.sock", std::process::id()))
}

/// The accept loop polls every ~100ms, so connecting can need a few tries.
fn connect_with_retry(path: &PathBuf) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return stream,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("could not connect to {}: {e}", path.display()),
        }
    }
}

fn read_line(reader: &mut BufReader<UnixStream>) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read protocol line");
    assert!(line.ends_with('\n'), "protocol lines are newline-framed");
    line.trim_end_matches('\n').to_string()
}

#[test]
fn hello_then_broadcast_events_arrive_as_protocol_json_lines() {
    let sock = temp_socket_path("events");
    let server = HudServer::new(&sock, "bottom").expect("bind HUD server");

    let stream = connect_with_retry(&sock);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);

    // hello arrives immediately on accept, before anything else.
    assert_eq!(
        read_line(&mut reader),
        format!(
            r#"{{"e":"hello","version":"{}","position":"bottom"}}"#,
            env!("CARGO_PKG_VERSION")
        )
    );

    // The accept thread registers the client right after writing hello; wait
    // until the server sees it before broadcasting.
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.client_count() == 0 {
        assert!(Instant::now() < deadline, "client never registered");
        std::thread::sleep(Duration::from_millis(10));
    }

    server.send(&HudEvent::listening(false));
    server.send(&HudEvent::Level { rms: 0.073 });
    server.send(&HudEvent::Done { words: 94, ms: 52 });

    assert_eq!(
        read_line(&mut reader),
        r#"{"e":"state","s":"listening","handsfree":false}"#
    );
    assert_eq!(read_line(&mut reader), r#"{"e":"level","rms":0.073}"#);
    assert_eq!(read_line(&mut reader), r#"{"e":"done","words":94,"ms":52}"#);

    // Drop removes the socket file (and says bye first).
    assert_eq!(server.client_count(), 1);
    drop(server);
    assert_eq!(read_line(&mut reader), r#"{"e":"bye"}"#);
    assert!(!sock.exists(), "socket file removed on Drop");
}

#[test]
fn stale_socket_file_is_replaced_on_bind() {
    let sock = temp_socket_path("stale");
    // Simulate a leftover socket from a crashed daemon.
    std::fs::write(&sock, b"stale").unwrap();

    let server = HudServer::new(&sock, "bottom").expect("bind over stale socket");
    let stream = connect_with_retry(&sock);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let hello = read_line(&mut reader);
    assert!(hello.starts_with(r#"{"e":"hello","#), "got: {hello}");
    drop(server);
}
