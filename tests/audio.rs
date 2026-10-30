// SPDX-License-Identifier: GPL-3.0-or-later
//! What the audio stream does with the frames a real device sent.
//!
//! The fixture is forty audio payloads captured from a real session, with each one's
//! length in front of it — the same shape `AIRPLAY_DUMP_AUDIO` writes. They are raw
//! AAC-ELD: no sync word and no sample rate, nothing a decoder could find a frame in on
//! its own, which is why this backend links GStreamer and pushes one buffer per frame.
//!
//! The sink is a file, and what it holds is the proof. Decoded AAC-ELD is PCM, so a file
//! with bytes in it is a decoder that read the frames; a file that is absent or empty is a
//! decoder that did not. That distinction matters because a push that succeeds only means
//! `appsrc` took the buffer — the decoder's opinion arrives on the bus a moment later, and
//! a test that stopped at "push returned Ok" would pass with caps that are wrong, which is
//! exactly the mistake a push-only test makes: it passes while nothing is decoded.

use rust_airplay::{AudioCodec, AudioConfig, AudioFrame, Renderer};
use rust_airplay_gstream::GstRenderer;

/// The frames of a real session, each with its length in front of it.
const FIXTURE: &[u8] = include_bytes!("fixtures/mirror-first-audio.eld");

/// Samples a frame, which is what the client's `spf` said the stream carries.
const SAMPLES_PER_FRAME: u64 = 480;

/// The fixture as payloads.
fn frames() -> Vec<&'static [u8]> {
    let mut frames = Vec::new();
    let mut at = 0usize;
    while at + 4 <= FIXTURE.len() {
        let length = u32::from_le_bytes([
            FIXTURE[at],
            FIXTURE[at + 1],
            FIXTURE[at + 2],
            FIXTURE[at + 3],
        ]) as usize;
        at += 4;
        if at + length > FIXTURE.len() {
            break;
        }
        frames.push(&FIXTURE[at..at + length]);
        at += length;
    }
    frames
}

#[test]
#[ignore = "needs a GStreamer runtime and its decoders; see README.md"]
fn a_raw_aac_eld_stream_is_decoded() {
    let captured = frames();
    assert!(
        captured.len() >= 40,
        "the fixture holds {} frames, which is too few to be a stream",
        captured.len()
    );

    // A file sink rather than a sound card, so this runs on a machine with no audio, and
    // so the decoded bytes can be counted afterwards. The path is written with forward
    // slashes: a pipeline is parsed from a string, and a Windows path's backslashes are
    // escapes in it — a mistake worth not making twice.
    let path = std::env::temp_dir().join(format!("airplay-gst-audio-{}.pcm", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let location = path.display().to_string().replace('\\', "/");
    let sink = format!("filesink location={location}");
    let renderer = GstRenderer::with_sinks(&sink, "fakesink").expect("a renderer");
    let opened = std::time::Instant::now();
    let stream = renderer
        .open_audio(AudioConfig {
            codec: AudioCodec::AacEld,
            ..AudioConfig::default()
        })
        .expect("an audio stream opens");
    println!("the audio pipeline opened in {:?}", opened.elapsed());

    for (index, payload) in captured.iter().enumerate() {
        let timestamp = index as u64 * SAMPLES_PER_FRAME;
        if let Err(error) = stream.push(AudioFrame {
            data: payload.to_vec(),
            timestamp,
        }) {
            panic!(
                "frame {index} ({} bytes) was refused: {error}\n\
                 a pipeline that refuses a raw AAC-ELD frame is one whose caps do not say \
                 what the frames are",
                payload.len()
            );
        }
    }

    // The decoder works through what it was given, and only then is there anything to
    // read. Ending the stream is what flushes the rest of it, and waiting for that is
    // what close does: when it returns, the last frame has been decoded and written.
    stream.close().expect("the stream ends");
    drop(stream);

    let decoded = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    let _ = std::fs::remove_file(&path);
    let wanted = captured.len() as u64 * SAMPLES_PER_FRAME * 2 * 2; // stereo, 16-bit
    assert!(
        decoded > wanted / 2,
        "the decoder produced {decoded} bytes of what should be about {wanted}: \
         the frames did not decode, which means the caps do not describe them"
    );
}
