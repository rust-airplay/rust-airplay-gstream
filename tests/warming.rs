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
//! picture looks — only about what opening a stream costs and whether the second one pays
//! for it again.

use std::time::Instant;

use rust_airplay::{Renderer, VideoConfig, VideoFrame};
use rust_airplay_gstream::GstRenderer;

fn opener() -> GstRenderer {
    GstRenderer::with_sinks("fakesink", "fakesink").expect("a renderer over fakesinks")
}

/// The frames of a real session, which is what a stream has to be given before it is
/// closed: the pipeline type-finds its input, and an empty pipe is something it reports
/// as an error rather than as a stream that simply had nothing in it.
const FIXTURE: &[u8] = include_bytes!("fixtures/mirror-first-frames.h264");

fn play_one_frame(stream: &dyn rust_airplay::VideoStream) {
    stream
        .push(VideoFrame {
            data: FIXTURE[..1_024].to_vec(),
            timestamp: 0,
            keyframe: true,
        })
        .expect("a frame");
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn a_warm_stream_opens_without_starting_a_pipeline() {
    // A cold open waits for the pipeline to say whether it is refused, and that wait is
    // 150 ms by design (see `Pipeline::settle`). A warm one has a pipeline already running
    // and has nothing to wait for.
    let cold = opener();
    let started = Instant::now();
    let first = cold
        .open_video(VideoConfig {
            width: None,
            height: None,
            codec: rust_airplay::VideoCodec::H264,
        })
        .expect("a video stream");
    let cold_open = started.elapsed();
    play_one_frame(&*first);
    first.close().expect("the stream ends");

    let warm = opener();
    warm.warm_video().expect("a warm pipeline starts");
    assert!(warm.video_is_warm(), "warming did not keep a pipeline");
    let started = Instant::now();
    let second = warm
        .open_video(VideoConfig {
            width: None,
            height: None,
            codec: rust_airplay::VideoCodec::H264,
        })
        .expect("a video stream");
    let warm_open = started.elapsed();

    assert!(
        warm_open * 2 < cold_open,
        "a warm stream took {warm_open:?} and a cold one {cold_open:?}: warming is not \
         reusing the pipeline, so every connection still pays for a new one"
    );

    // A warm stream must not end the pipeline when its session does: the next client would
    // then pay the very cost warming exists to avoid.
    play_one_frame(&*second);
    second.close().expect("the session ends");
    assert!(
        warm.video_is_warm(),
        "closing a session ended the warm pipeline"
    );
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn a_warm_pipeline_plays_two_sessions_in_a_row() {
    // Reuse has to be more than cheaper: the second session's frames must actually reach
    // the decoder. A warm pipeline is one process that has already seen one stream, and a
    // stream that only starts when its session does would never notice the difference.
    let renderer = opener();
    renderer.warm_video().expect("a warm pipeline");
    for session in 0..2 {
        let stream = renderer
            .open_video(VideoConfig {
                width: None,
                height: None,
                codec: rust_airplay::VideoCodec::H264,
            })
            .unwrap_or_else(|error| panic!("session {session} could not open: {error}"));
        for index in 0..30u64 {
            stream
                .push(VideoFrame {
                    data: FIXTURE[..1_024].to_vec(),
                    timestamp: index * 1_000_000_000 / 30,
                    keyframe: index == 0,
                })
                .unwrap_or_else(|error| {
                    panic!("session {session} frame {index} was refused: {error}")
                });
        }
        stream
            .close()
            .unwrap_or_else(|error| panic!("session {session} ended badly: {error}"));
        assert!(
            renderer.video_is_warm(),
            "session {session} ended the pipeline the next one needs"
        );
    }
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn a_cold_stream_ends_its_pipeline_when_the_session_does() {
    // The other half of the rule: without warming, a session owns its pipeline, and a
    // window that outlived its session would be a window nobody can close.
    let renderer = opener();
    assert!(!renderer.video_is_warm(), "a fresh renderer is not warm");
    let stream = renderer
        .open_video(VideoConfig {
            width: None,
            height: None,
            codec: rust_airplay::VideoCodec::H264,
        })
        .expect("a video stream");
    play_one_frame(&*stream);
    stream.close().expect("the stream ends");
    assert!(!renderer.video_is_warm(), "a cold renderer kept nothing");
    let reopened = renderer
        .open_video(VideoConfig {
            width: None,
            height: None,
            codec: rust_airplay::VideoCodec::H264,
        })
        .expect("a second stream starts its own pipeline");
    play_one_frame(&*reopened);
    reopened.close().expect("the session ends");
}
