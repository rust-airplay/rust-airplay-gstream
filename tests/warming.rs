// SPDX-License-Identifier: GPL-3.0-or-later
//! What a video stream costs to open, and what warming one changes.
//!
//! Starting a pipeline is the largest single cost between a client tapping a receiver and
//! its first picture, and it is not this crate's own work: a sink that puts a picture on
//! screen spends 702 ms coming up *before it has been given a frame*, because the window
//! and the graphics device are created when the pipeline starts. A session that starts its
//! pipeline when the client connects pays that while the client is already sending.
//!
//! These tests use `fakesink`, so they need no display and assert nothing about how a
//! picture looks -- only about what opening a stream costs and whether the second one pays
//! for it again. One of them uses a file sink instead, because a warm pipeline that is
//! cheaper but decodes nothing is not a warm pipeline.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rust_airplay::{Renderer, VideoCodec, VideoConfig, VideoFrame, VideoStream};
use rust_airplay_gstream::{GstRenderer, Options};

/// The frames of a session, which is what a stream has to be given before it is closed:
/// the pipeline type-finds its input, and an empty pipe is something it reports as an
/// error rather than as a stream that simply had nothing in it.
const FIXTURE: &[u8] = include_bytes!("fixtures/mirror-first-frames.h264");

/// A renderer over `fakesink`, warm or not.
fn opener(warm: bool) -> GstRenderer {
    GstRenderer::with_options(Options::new("fakesink", "fakesink").with_warm_video(warm))
        .expect("a renderer over fakesinks")
}

/// A renderer whose video pipeline decodes into a file, so that what it decoded can be
/// counted afterwards.
fn recorder(path: &Path) -> GstRenderer {
    let _ = std::fs::remove_file(path);
    // A pipeline is parsed from a string, and a Windows path's backslashes are escapes in
    // it, so the path is handed over with forward slashes.
    let location = path.display().to_string().replace('\\', "/");
    let sink = format!("filesink location={location}");
    GstRenderer::with_options(Options::new("fakesink", &sink).with_warm_video(true))
        .expect("a renderer that records")
}

fn temp_file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("airplay-gst-{name}-{}.raw", std::process::id()))
}

fn play_one_frame(stream: &dyn VideoStream) {
    stream
        .push(VideoFrame {
            data: FIXTURE[..1_024].to_vec(),
            timestamp: 0,
            keyframe: true,
        })
        .expect("a frame");
}

fn play_thirty(stream: &dyn VideoStream) {
    for index in 0..30u64 {
        stream
            .push(VideoFrame {
                data: FIXTURE[..1_024].to_vec(),
                timestamp: index * 33_333_333,
                keyframe: index == 0,
            })
            .unwrap_or_else(|error| panic!("frame {index} was refused: {error}"));
    }
}

fn video() -> VideoConfig {
    VideoConfig {
        width: None,
        height: None,
        codec: VideoCodec::H264,
    }
}

/// Waits for `path` to hold more than `before` bytes, and returns what it holds.
///
/// Decoding is asynchronous: a push hands bytes to a pipe and returns, so a file that has
/// not grown *yet* says nothing. This waits instead of sleeping a fixed time, because a
/// fixed sleep is either too short on a slow machine or wasted on a fast one.
fn grown(path: &Path, before: u64) -> u64 {
    for _ in 0..100 {
        let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        if size > before {
            return size;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "{} did not grow past {before} bytes in five seconds: the frames were accepted 
         and never decoded",
        path.display()
    );
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn a_warm_stream_opens_without_starting_a_pipeline() {
    // One renderer, two sessions, so the difference measured is the pipeline and not the
    // machine. The first open pays for the pipeline -- 150 ms of it is the settle that
    // waits to hear whether the description was refused -- and keeps it; the second open
    // adopts it and has nothing to wait for.
    let renderer = opener(true);
    assert!(
        !renderer.video_is_warm(),
        "a warm renderer starts empty: nothing is warmed until a session has a pipeline"
    );
    let started = Instant::now();
    let first = renderer.open_video(video()).expect("a video stream");
    let cold_open = started.elapsed();
    play_one_frame(&*first);
    first.close().expect("the stream ends");
    assert!(renderer.video_is_warm(), "warming did not keep a pipeline");

    let started = Instant::now();
    let second = renderer.open_video(video()).expect("a second video stream");
    let warm_open = started.elapsed();
    play_one_frame(&*second);

    assert!(
        warm_open * 2 < cold_open,
        "a warm stream took {warm_open:?} and a cold one {cold_open:?}: warming is not 
         reusing the pipeline, so every connection still pays for a new one"
    );

    // A warm stream must not end the pipeline when its session does: the next client would
    // then pay the very cost warming exists to avoid.
    second.close().expect("the session ends");
    assert!(
        renderer.video_is_warm(),
        "closing a session ended the warm pipeline"
    );
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn a_warm_pipeline_decodes_two_sessions_in_a_row() {
    // Reuse has to be more than cheaper: the second session's frames must actually reach
    // the decoder. The sink is a file, and every byte in it was decoded -- a warm pipeline
    // that accepted frames and produced nothing would grow no file.
    let path = temp_file("warm-two-sessions");
    let renderer = recorder(&path);
    for session in 0..2 {
        let before = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let stream = renderer
            .open_video(video())
            .unwrap_or_else(|error| panic!("session {session} could not open: {error}"));
        play_thirty(&*stream);
        let after = grown(&path, before);
        println!("session {session}: {before} -> {after} bytes decoded");
        stream
            .close()
            .unwrap_or_else(|error| panic!("session {session} ended badly: {error}"));
        assert!(
            renderer.video_is_warm(),
            "session {session} ended the pipeline the next one needs"
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn a_cold_stream_ends_its_pipeline_when_the_session_does() {
    // The other half of the rule: without warming, a session owns its pipeline, and a
    // window that outlived its session would be a window nobody can close.
    let renderer = opener(false);
    assert!(!renderer.video_is_warm(), "a fresh renderer is not warm");
    let stream = renderer.open_video(video()).expect("a video stream");
    play_one_frame(&*stream);
    stream.close().expect("the stream ends");
    assert!(!renderer.video_is_warm(), "a cold renderer kept nothing");
    let reopened = renderer
        .open_video(video())
        .expect("a second stream starts its own pipeline");
    play_one_frame(&*reopened);
    reopened.close().expect("the session ends");
}
