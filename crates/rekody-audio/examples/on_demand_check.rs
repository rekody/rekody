//! Hardware check for on-demand mic stream lifecycle.
//!
//! Run with `cargo run -p rekody-audio --example on_demand_check`.
//! Watch the macOS menu-bar mic indicator: it must be OFF during the idle
//! phases and ON only during the two recording phases.

use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let config = rekody_audio::AudioConfig {
        vad_threshold: 0.0001, // accept ambient noise so segments emit
        record_all_audio: true,
    };
    let capture = rekody_audio::AudioCapture::new(config.clone());
    let mut live_rx = capture.live_chunks();
    let mut segment_rx = capture.open(config)?;
    println!("== opened (idle) — mic indicator must be OFF for 4s ==");
    tokio::time::sleep(Duration::from_secs(4)).await;

    for round in 1..=2 {
        println!("== round {round}: recording 3s — indicator must be ON ==");
        capture.start_recording();
        let mut live_samples = 0usize;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            tokio::select! {
                chunk = live_rx.recv() => {
                    if let Some(c) = chunk { live_samples += c.len(); }
                }
                _ = tokio::time::sleep_until(deadline) => {}
            }
        }
        capture.stop_recording();
        let segment = tokio::time::timeout(Duration::from_secs(2), segment_rx.recv()).await;
        let seg_desc = match &segment {
            Ok(Some(s)) => format!("{} samples ({:.2}s)", s.samples.len(), s.duration_secs),
            Ok(None) => "channel closed".into(),
            Err(_) => "TIMEOUT — no segment".into(),
        };
        println!("== round {round} done: live tap {live_samples} samples, segment: {seg_desc} ==");
        println!("== idle 4s — indicator must be OFF ==");
        tokio::time::sleep(Duration::from_secs(4)).await;
    }

    capture.shutdown();
    println!("== shutdown ==");
    Ok(())
}
