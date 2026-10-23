// SPDX-License-Identifier: GPL-3.0-or-later
//! What this backend's own pipeline does with frames a device actually sent.
//!
//! The bytes are real: the first frames of an iPad's mirror stream, captured from a
//! session and kept under `tests/fixtures/`. They decode completely on their own, so
//! if a pipeline this crate builds does not decode all of them, the difference is the
//! pipeline and not the stream.
//!
//! The count is what is asserted, because a picture cannot be: every slice a client
//! sent is one frame, and a pipeline that loses one shows a partly decoded screen.

use std::io::{Read, Write};

/// The frames a device sent, as one Annex-B stream.
const DUMP: &[u8] = include_bytes!("fixtures/mirror-first-frames.h264");

/// The bytes of one decoded frame of this stream, in I420 at 1554x1080.
const FRAME_BYTES: usize = 1554 * 1080 * 3 / 2;

/// The start code every unit of an Annex-B stream is behind.
const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// The codec packet a device opens its stream with, exactly as it sent it.
///
/// Thirty-seven bytes of AVCDecoderConfigurationRecord: version, profile, level, a
/// length prefix of four bytes, one sequence set of eighteen bytes and one picture
/// set of four. It is what a session hands a decoder *before* the first frame, and it
/// is not an Annex-B stream: there is no start code anywhere in it. A test that
/// decoded the frames on their own therefore proved less than it looked like it did,
/// because a session never sends them on their own.
const CODEC_PACKET: [u8; 37] = [
    0x01, 0x64, 0x00, 0x28, 0xff, 0xe1, 0x00, 0x12, 0x27, 0x64, 0x00, 0x28, 0xac, 0x13, 0x14, 0x50,
    0x18, 0x80, 0x89, 0xe2, 0x25, 0x9b, 0x80, 0x86, 0x80, 0x81, 0x01, 0x00, 0x04, 0x28, 0xee, 0x3c,
    0xb0, 0x02, 0x00, 0x00, 0x00,
];

/// The bytes a session actually hands a decoder: the codec packet, then the frames.
fn session_bytes() -> Vec<u8> {
    let mut bytes = CODEC_PACKET.to_vec();
    bytes.extend_from_slice(DUMP);
    bytes
}

/// The program to run pipelines in, if this machine has one.
fn launcher() -> Option<String> {
    match rust_airplay_gstream::pipeline::find_launcher() {
        Ok(program) => Some(program),
        Err(error) => {
            eprintln!("skipping: {error}");
            None
        }
    }
}

/// How many frames the fixture holds.
///
/// Not the number of units: a slice is one frame, but the parameter sets around it
/// are units too, and counting those as frames is how this test first reported a
/// pipeline that lost seven frames when it had lost none. The count comes from the
/// dump being thirty frames of a session, and the decoded size agreeing with it.
fn frames_in_fixture() -> usize {
    let mut frames = 0;
    let mut at = 0;
    while at + 4 <= DUMP.len() {
        if DUMP[at..at + 4] == START_CODE {
            // The unit behind the start code: a slice is a frame, a parameter set is
            // not.
            let unit = DUMP.get(at + 4).map_or(0, |byte| byte & 0x1f);
            if unit != 7 && unit != 8 {
                frames += 1;
            }
            at += 4;
        } else {
            at += 1;
        }
    }
    frames
}

/// Runs a pipeline over the fixture and returns how many decoded frames came out.
///
/// Frames are written to the pipeline's standard input the way the backend writes
/// them — as they arrived, one after another, with no framing of the receiver's own
/// — and its standard output is the raw decoded stream, so the count is the
/// pipeline's own answer and not a receiver's.
fn decode_with(program: &str, pipeline: &str) -> usize {
    let session = session_bytes();
    let mut child = std::process::Command::new(program)
        .arg("-q")
        // Handed over the way the backend hands it: as separate arguments, because a
        // caps filter holds commas and one argument holding a whole pipeline is the
        // form Windows quoting breaks.
        .args(pipeline.split_whitespace())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("a pipeline");
    let mut input = child.stdin.take().expect("a pipe to write frames to");
    let writer = std::thread::spawn(move || {
        let _ = input.write_all(&session);
        // Dropping the pipe is the end of stream, which is how a sink knows to play
        // what it has left before it exits.
    });
    let mut decoded = Vec::new();
    child
        .stdout
        .take()
        .expect("the pipeline's output")
        .read_to_end(&mut decoded)
        .expect("the decoded frames");
    writer.join().expect("the writer");
    let outcome = child.wait_with_output().expect("the pipeline ended");
    let said = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        outcome.status.success(),
        "the pipeline failed after {} decoded bytes: {said}",
        decoded.len()
    );
    decoded.len() / FRAME_BYTES
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn the_pipeline_decodes_every_frame_a_device_sent() {
    let Some(program) = launcher() else {
        return;
    };
    // The pipeline is the one `GstRenderer` builds for a video stream, with its sink
    // replaced by the decoded bytes so that what it produced can be counted.
    let pipeline = "fdsrc ! decodebin ! videoconvert ! video/x-raw,format=I420 ! fdsink";
    let frames = decode_with(&program, pipeline);
    let expected = frames_in_fixture();
    println!("decodebin: {frames} frames, fixture holds {expected}");
    assert!(
        frames > 0,
        "the pipeline decoded nothing from {} bytes",
        DUMP.len()
    );
    // The last frame may still be inside the decoder when the stream ends, so one
    // fewer is expected and two fewer is a lost frame.
    assert!(
        frames + 1 >= expected,
        "the pipeline decoded {frames} of {expected} frames: {} were lost",
        expected - frames
    );
}

#[test]
#[ignore = "needs a GStreamer runtime; see README.md"]
fn the_decoder_a_player_would_pick_decodes_the_same() {
    // The control. If this and the pipeline above disagree, the difference is which
    // decoder each one chose, and that is the thing worth knowing.
    let Some(program) = launcher() else {
        return;
    };
    let pipeline =
        "fdsrc ! h264parse ! avdec_h264 ! videoconvert ! video/x-raw,format=I420 ! fdsink";
    let frames = decode_with(&program, pipeline);
    println!(
        "h264parse: {frames} frames, fixture holds {}",
        frames_in_fixture()
    );
    assert!(frames > 0, "the control pipeline decoded nothing");
}
