// SPDX-License-Identifier: GPL-3.0-or-later
//! What video pacing does with a real clock.
//!
//! The arithmetic is unit-tested against a clock that does not sleep (see
//! `src/pacing.rs`); this is the part a unit test cannot see: that a stream borrowed from a
//! real renderer waits on a cumulative timeline, and that turning pacing off turns the waiting
//! off. The test calls `set_pace` directly on purpose: the receiver stopped calling it in core
//! 0.3.1, so the hook's behaviour is only observable through a caller that drives it.

use std::time::{Duration, Instant};

use rust_airplay::{Renderer, VideoConfig, VideoFrame};
use rust_airplay_gstream::{GstRenderer, Options};

/// A frame's worth of Annex-B bytes.
///
/// Pacing does not decode anything, so the content only has to be a plausible buffer.
const FRAME: &[u8] = &[
    0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x21, 0xff, 0x00, 0x11, 0x22,
];

fn video() -> VideoConfig {
    VideoConfig {
        width: None,
        height: None,
        codec: rust_airplay::VideoCodec::H264,
    }
}

fn renderer(pacing: bool) -> GstRenderer {
    GstRenderer::with_options(Options::new("fakesink", "fakesink").with_pacing(pacing))
        .expect("a renderer over fakesinks")
}

/// 50 ms rather than 16: a sleep below the platform's timer granularity cannot be
/// scheduled, and the assertion below is about the timeline, not about how fine the
/// system's timer is.
const INTERVAL: Duration = Duration::from_millis(50);

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn pacing_keeps_a_cumulative_timeline() {
    let renderer = renderer(true);
    let stream = renderer.open_video(video()).expect("a video stream");
    let frames = 40u32;

    let started = Instant::now();
    for _ in 0..frames {
        stream.set_pace(INTERVAL.as_secs_f64()).expect("a spacing");
        stream
            .push(VideoFrame {
                data: FRAME.to_vec(),
                timestamp: 0,
                keyframe: false,
            })
            .expect("a frame");
    }
    let elapsed = started.elapsed();

    // The first call anchors the grid and waits for nothing; the remaining ones wait for
    // their place on it. Lateness from a sleep granularity must not accumulate, so the
    // total is the grid and not the sum of the sleeps.
    let ideal = INTERVAL * (frames - 1);
    let drift = elapsed.saturating_sub(ideal);
    println!("{frames} frames took {elapsed:?}, the timeline is {ideal:?}, drift {drift:?}");
    assert!(
        elapsed + INTERVAL / 2 >= ideal,
        "{frames} frames took {elapsed:?}, which is faster than the timeline {ideal:?}"
    );
    assert!(
        drift < INTERVAL,
        "the timeline drifted {drift:?} over {frames} frames, more than one interval"
    );
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn pacing_that_is_off_does_not_wait() {
    let renderer = renderer(false);
    let stream = renderer.open_video(video()).expect("a video stream");
    let started = Instant::now();
    for _ in 0..3 {
        stream.set_pace(0.5).expect("a spacing");
    }
    let elapsed = started.elapsed();
    println!("three half-second spacings took {elapsed:?} with pacing off");
    assert!(
        elapsed < Duration::from_millis(50),
        "pacing off still waited {elapsed:?}"
    );
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn a_zero_spacing_does_not_wait_even_with_pacing_on() {
    // The core means "do not pace" by 0.0; it must not mean "sleep nothing" once and then
    // treat the next real spacing as overdue.
    let renderer = renderer(true);
    let stream = renderer.open_video(video()).expect("a video stream");
    let started = Instant::now();
    for _ in 0..3 {
        stream.set_pace(0.0).expect("no spacing");
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(50),
        "a zero spacing waited {elapsed:?}"
    );
}
