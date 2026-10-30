// SPDX-License-Identifier: GPL-3.0-or-later
//! What the video path costs per frame.
//!
//! Not a gate: a rate printed by a machine is a fact about that machine, and failing a
//! run because a shared runner was busy would teach nobody anything. It is here so a
//! change to the pipeline, the writer or the frame hand-off can be measured against the
//! same stream instead of being argued about.
//!
//! ```sh
//! cargo test --test throughput -- --ignored --nocapture
//! ```

use std::time::Instant;

use rust_airplay::VideoCodec;
use rust_airplay_gstream::testing;

/// The frames of the stream, as one Annex-B stream.
const DUMP: &[u8] = include_bytes!("fixtures/mirror-first-frames.h264");

/// How many times the fixture is replayed.
///
/// Ten replays is 480 pictures: long enough for the pipeline start not to dominate, and
/// short enough that the test is over in a fraction of a second.
const REPLAYS: usize = 10;

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn the_video_path_keeps_up_with_a_mirror_stream() {
    let Ok(program) = testing::launcher() else {
        eprintln!("skipping: no gst-launch-1.0");
        return;
    };
    // fakesink: the same decode work, none of the disk. A file sink would write 480
    // frames of I420, which is 160 MB and says nothing extra about the path.
    let args = testing::video_args(VideoCodec::H264, "fakesink")
        .expect("the arguments of a video pipeline");
    let mut stream = Vec::with_capacity(DUMP.len() * REPLAYS);
    for _ in 0..REPLAYS {
        stream.extend_from_slice(DUMP);
    }
    let pictures = 48 * REPLAYS;

    let started = Instant::now();
    testing::feed(&program, &args, &stream).expect("the pipeline played the stream");
    let elapsed = started.elapsed();

    let frames_per_second = pictures as f64 / elapsed.as_secs_f64();
    let per_frame_ms = elapsed.as_secs_f64() * 1e3 / pictures as f64;
    println!(
        "{pictures} pictures in {elapsed:?} = {frames_per_second:.0} frames/s ({per_frame_ms:.2} ms a frame, pipeline start included)"
    );
    // 30 frames a second is what a mirror sends; a path that cannot manage half of that
    // on a machine with nothing else to do will not keep up in a session.
    assert!(
        frames_per_second > 15.0,
        "the video path managed {frames_per_second:.1} frames/s, which cannot keep up"
    );
}
