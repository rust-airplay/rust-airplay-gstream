// SPDX-License-Identifier: GPL-3.0-or-later
//! What the backend does with a real GStreamer runtime.
//!
//! These are `#[ignore]`d because they need one: a machine with no `gst-launch-1.0`
//! cannot run them, and this crate is built on machines that have none. `README.md` says
//! how to run them.
//!
//! What they check is the part of the backend that is not a thin wrapper: that the
//! pipelines it builds are pipelines GStreamer accepts, that a coding it describes is a
//! coding the runtime can find a decoder for, and that bytes written to a pipe are what
//! the pipeline reads. `fakesink` is the sink for that, because it accepts everything and
//! needs no device.

use rust_airplay::{AudioCodec, AudioConfig, Renderer, VideoConfig, VideoFrame, Volume};
use rust_airplay_gstream::{GstRenderer, pipeline};

/// This machine's renderer, or `None` when it has no runtime to render with.
fn runtime() -> Option<GstRenderer> {
    match GstRenderer::new() {
        Ok(renderer) => Some(renderer),
        Err(error) => {
            eprintln!("skipping: {error}");
            None
        }
    }
}

/// The program to run pipelines in, if this machine has one.
fn launcher() -> Option<String> {
    match pipeline::find_launcher() {
        Ok(program) => Some(program),
        Err(error) => {
            eprintln!("skipping: {error}");
            None
        }
    }
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn a_launcher_is_found_and_a_renderer_starts() {
    let Some(renderer) = runtime() else {
        return;
    };
    assert!(
        !renderer.program().is_empty(),
        "a renderer names its program"
    );
    assert!(
        !renderer.audio_sink().is_empty() && !renderer.video_sink().is_empty(),
        "and the sinks it will use"
    );
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn every_coding_of_the_boundary_has_a_description_the_runtime_accepts() {
    let Some(renderer) = runtime() else {
        return;
    };
    // A description the runtime refuses fails at once, before any frame is written, so a
    // coding this backend cannot describe is caught here rather than in a session that
    // silently has no audio.
    for codec in [AudioCodec::Alac, AudioCodec::AacEld] {
        let config = AudioConfig {
            codec,
            ..AudioConfig::default()
        };
        let Err(error) = renderer.open_audio(config) else {
            continue;
        };
        let text = error.to_string();
        assert!(
            !text.contains("erroneous pipeline"),
            "{codec:?}'s description was refused by gst-launch itself: {text}"
        );
        // Anything else is this machine lacking a decoder for it, which the architecture document calls an
        // environment problem and this test reports rather than fails on.
        eprintln!("{codec:?} has no decoder on this machine: {text}");
    }
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn a_wav_signal_written_to_the_pipe_comes_out_the_other_side() {
    // The mechanism the whole backend rests on: bytes written to a pipeline's standard
    // input are what its source reads. `wavparse` is what proves they arrived intact,
    // since a WAV header a parser can read is one the pipe carried whole.
    let Some(program) = launcher() else {
        return;
    };
    let wav = wav_file(&pcm_sine(44_100));
    let description = "fdsrc ! audio/x-wav ! wavparse ! audioconvert ! fakesink sync=false";
    let pipe = pipeline::Pipeline::start(&program, description).expect("a pipeline");
    pipe.write(&wav).expect("the signal");
    pipe.finish()
        .expect("the pipeline played every byte it was given");
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn the_video_pipeline_accepts_annex_b_units() {
    let Some(renderer) = runtime() else {
        return;
    };
    let stream = renderer
        .open_video(VideoConfig {
            width: Some(640),
            height: Some(360),
            codec: rust_airplay::VideoCodec::H264,
        })
        .expect("a video pipeline");
    // A parameter set behind a start code: not a picture, but the parser sees a unit in
    // it, which is what says the caps and the parser are the right ones.
    let unit = [
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x00, 0x00, 0x01, 0x68, 0xce,
    ];
    if let Err(error) = stream.push(VideoFrame {
        data: unit.to_vec(),
        timestamp: 1_000_000,
        keyframe: false,
    }) {
        let text = error.to_string();
        assert!(
            !text.contains("erroneous pipeline"),
            "the pipeline description itself was refused: {text}"
        );
        eprintln!("this machine's video pipeline cannot take a unit: {text}");
    }
    // A frame that is not a picture leaves a decoder waiting for one, so the stream is
    // flushed rather than closed.
    let _ = stream.flush();
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn a_sink_that_does_not_exist_is_reported_when_the_stream_is_opened() {
    // The failure a user is most likely to hit is a sink their machine does not have, and
    // it has to arrive when a stream is opened rather than as silence.
    let Ok(renderer) = GstRenderer::with_sinks("no-such-sink-name", "fakesink") else {
        eprintln!("skipping: no gst-launch-1.0");
        return;
    };
    let error = renderer
        .open_audio(AudioConfig::default())
        .expect_err("a sink that does not exist");
    let text = error.to_string();
    assert!(
        text.contains("no-such-sink-name") || text.contains("no element"),
        "the reason names what was missing: {text}"
    );
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn the_volume_a_session_sets_is_read_back() {
    let Some(renderer) = runtime() else {
        return;
    };
    assert_eq!(renderer.volume(), Volume::FULL);
    renderer.set_volume(Volume::new(0.5)).expect("a volume");
    assert_eq!(renderer.volume().level(), 0.5);
}

/// A sine wave as 16-bit little-endian samples, so a parser has something real to read.
fn pcm_sine(samples: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples as usize * 2);
    for index in 0..samples {
        let phase = (index as f64) * 2.0 * std::f64::consts::PI * 440.0 / 44_100.0;
        let value = (phase.sin() * 10_000.0) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// A WAV file around `samples`, which is what makes the bytes self-describing.
fn wav_file(samples: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() + 44);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((samples.len() + 36) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&44_100u32.to_le_bytes());
    out.extend_from_slice(&88_200u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    out.extend_from_slice(samples);
    out
}
