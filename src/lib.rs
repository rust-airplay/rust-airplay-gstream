// SPDX-License-Identifier: GPL-3.0-or-later
//! A renderer that plays through GStreamer.
//!
//! The backend opens two pipelines, one per medium, and the application chooses the
//! sinks. **Audio is linked; video is a subprocess**, and the split follows from what
//! the two media allow:
//!
//! - **Video** is run as `gst-launch-1.0` with Annex-B frames written to its standard
//!   input. H.264 and H.265 carry start codes, so a decoder finds its own picture
//!   boundaries in a byte stream, and a decoder that crashes on one frame takes only
//!   its own process with it.
//! - **Audio** cannot be: a raw AAC-ELD or ALAC frame has no sync word, so a byte
//!   stream says nothing about where a frame ends. It needs one buffer per frame --
//!   `appsrc` -- which is why this backend links `libgstreamer` for audio.
//!
//! ```text
//! audio: appsrc ! queue ! avdec_aac ! audioconvert ! audioresample ! volume ! <sink>
//! video: fdsrc ! h264parse ! avdec_h264 ! <sink>
//! ```
//!
//! # Sinks
//!
//! A sink is named by an element and, optionally, its properties -- `fakesink`, or
//! `filesink location=out.raw`. The element must be installed; the properties are passed
//! to it. [AUDIO_SINKS] and [VIDEO_SINKS] are the ones this backend prefers, tried in
//! order, and [Options] can name another; the environment variables [AUDIO_SINK_ENV] and
//! [VIDEO_SINK_ENV] do the same without code.
//!
//! # Time
//!
//! Neither medium is scheduled against the client's clock: an AirPlay timestamp is an
//! RTP counter whose origin means nothing here, and a display's clock is not the
//! sender's. Audio is timed from its own first frame, and video is paced by the caller
//! when it asks for it ([Options] and [Chain]).
//!
//! # Flushing
//!
//! A flush takes the audio pipeline back to READY, which is what empties a queue, and
//! starts it again. For video it replaces the decoder process, because a decoder cannot
//! otherwise be made to forget what it queued; the key frame a client sends after a flush
//! is what makes the new process correct.

#![deny(missing_docs)]

mod audio;
mod options;
mod pipeline;
mod renderer;
mod video;

pub use crate::options::{
    AUDIO_SINK_ENV, AUDIO_SINKS, Chain, MAX_PACE_SECONDS, MAX_VIDEO_FRAME_BYTES, Options,
    VIDEO_SINK_ENV, VIDEO_SINKS, WARM_VIDEO_ENV,
};
pub use crate::renderer::GstRenderer;

/// Helpers for this crate's own integration tests.
///
/// Not part of the API. These exist so that the tests drive the same code the renderer
/// does -- one pipeline implementation, one launcher lookup -- rather than a copy of it
/// that can drift.
#[doc(hidden)]
pub mod testing {
    use crate::options::{Options, sink_tokens};
    use crate::pipeline::{LaunchError, Pipeline, find_launcher};
    use crate::video;
    use rust_airplay::VideoCodec;

    /// Runs one pipeline with these arguments, feeds it these bytes, and closes it.
    ///
    /// Closing the input is the end of the stream, so this returns once the pipeline has
    /// played everything it was given and exited.
    ///
    /// # Errors
    ///
    /// Returns what went wrong as text: the program could not be started, it refused
    /// the description, or it stopped before the bytes were written.
    pub fn feed(program: &str, args: &[String], bytes: &[u8]) -> Result<(), String> {
        let pipeline = Pipeline::start(program, args).map_err(reason)?;
        if let Err(error) = pipeline.push(bytes) {
            pipeline.abort();
            return Err(reason(error));
        }
        pipeline.finish().map_err(reason)
    }

    /// Finds `gst-launch-1.0`, as a test that needs a program of its own looks for it.
    ///
    /// # Errors
    ///
    /// Returns why no program was found.
    pub fn launcher() -> Result<String, String> {
        find_launcher().map_err(reason)
    }

    /// The arguments this backend builds for a video stream of one coding, with a sink
    /// which is a test's own.
    ///
    /// The same builder the renderer uses, so that a test measuring a pipeline measures
    /// the one that decodes a session rather than a copy of it.
    ///
    /// # Errors
    ///
    /// Returns what the sink description was refused for.
    pub fn video_args(codec: VideoCodec, sink: &str) -> Result<Vec<String>, String> {
        // A sink is checked against the installed elements, which needs a runtime.
        let _ = gstreamer::init();
        let options = Options::new("fakesink", sink);
        let chain = options.chain(codec).map_err(|error| error.to_string())?;
        let tokens = sink_tokens(sink).map_err(|error| error.to_string())?;
        Ok(video::pipeline_args(chain, &tokens))
    }

    /// What the pipeline reported, or what was wrong with the attempt.
    fn reason(error: LaunchError) -> String {
        error.to_string()
    }
}
