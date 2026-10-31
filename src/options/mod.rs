// SPDX-License-Identifier: GPL-3.0-or-later
//! What the renderer is built from: sinks, decoder chains, and the environment.
//!
//! A sink or a chain is a fragment of pipeline description rather than a name, because
//! a sink usually needs an argument to be useful (a window to draw in, a file to write
//! to). The fragment is not pasted into a description unchecked: what a fragment may
//! name is a real element and a list of property=value pairs, nothing else, so a caller
//! cannot smuggle a second branch into the pipeline it was given (see
//! [Options] and [crate::GstRenderer::with_options]).

mod sink;

pub(crate) use sink::{resolve_sink, sink_tokens};

use rust_airplay::{RenderError, VideoCodec};

/// The audio sinks this crate picks from when the environment names none.
pub const AUDIO_SINKS: &[&str] = &[
    "wasapisink",
    "autoaudiosink",
    "osssink",
    "alsasink",
    "pulsesink",
    "fakesink",
];

/// The video sinks this crate picks from when the environment names none.
pub const VIDEO_SINKS: &[&str] = &["d3d11videosink", "autovideosink", "glimagesink", "fakesink"];

/// Environment variable naming the audio sink, as a pipeline fragment.
pub const AUDIO_SINK_ENV: &str = "AIRPLAY_GST_AUDIO_SINK";

/// Environment variable naming the video sink, as a pipeline fragment.
pub const VIDEO_SINK_ENV: &str = "AIRPLAY_GST_VIDEO_SINK";

/// Environment variable asking for a video pipeline kept between sessions.
pub const WARM_VIDEO_ENV: &str = "AIRPLAY_GST_VIDEO_WARM";

/// The largest spacing this backend will pace a video stream at.
///
/// A guard, not a policy: the receiver's own lead is tens of milliseconds, and a spacing
/// above this is a client that stopped rather than a slow one, so clamping keeps a stale
/// value from holding a frame back for a second. A value past the guard is not an error;
/// pacing is a hint, and the next frame arrives when it arrives.
pub const MAX_PACE_SECONDS: f64 = 1.0;

/// The largest video frame this backend accepts.
///
/// A 4K key frame is a few megabytes; a buffer far past that is not a frame a client
/// sent, it is a bug or an attack, and the difference is worth saying out loud rather
/// than handing to a decoder.
pub const MAX_VIDEO_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// The elements one coding is decoded by.
///
/// Kept per coding rather than shared, because H.264 and H.265 need different parsers
/// and a session can change coding between streams: a chain that named H.264 for both
/// would show an H.265 stream as nothing at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    parser: String,
    decoder: String,
}

impl Chain {
    /// A chain from a parser and a decoder, both given as pipeline fragments.
    pub fn new(parser: impl Into<String>, decoder: impl Into<String>) -> Self {
        Self {
            parser: parser.into(),
            decoder: decoder.into(),
        }
    }

    /// The elements that turn one access unit into one frame.
    pub fn parser(&self) -> &str {
        &self.parser
    }

    /// The element that turns one access unit into one frame.
    pub fn decoder(&self) -> &str {
        &self.decoder
    }
}

impl Default for Chain {
    fn default() -> Self {
        h264_chain()
    }
}

/// What H.264 is decoded by, which is also what most sessions send.
fn h264_chain() -> Chain {
    Chain::new("h264parse", "avdec_h264")
}

/// What H.265 is decoded by.
fn h265_chain() -> Chain {
    Chain::new("h265parse", "avdec_h265")
}

/// How a renderer is configured.
///
/// The default reads the environment: the sink variables exist so a machine this
/// backend guessed wrong about can be corrected without a rebuild, and the warm
/// variable because keeping a pipeline alive costs a process per renderer. A sink
/// named here is a fragment of pipeline description checked when the renderer is
/// built, not one invented at the first frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    audio_sink: Option<String>,
    video_sink: Option<String>,
    h264: Chain,
    h265: Chain,
    warm_video: bool,
    pacing: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            audio_sink: named(AUDIO_SINK_ENV),
            video_sink: named(VIDEO_SINK_ENV),
            h264: h264_chain(),
            h265: h265_chain(),
            warm_video: warming_from(named(WARM_VIDEO_ENV).as_deref()),
            pacing: true,
        }
    }
}

impl Options {
    /// Sinks named explicitly, and nothing read from the environment.
    ///
    /// This is what a test uses: a run that wrote to whatever the machine's environment
    /// said would test the machine. Warming is off here and only
    /// [Options::with_warm_video] turns it on, so `AIRPLAY_GST_VIDEO_WARM` cannot change
    /// what this constructor builds.
    pub fn new(audio_sink: impl Into<String>, video_sink: impl Into<String>) -> Self {
        Self {
            audio_sink: Some(audio_sink.into()),
            video_sink: Some(video_sink.into()),
            h264: h264_chain(),
            h265: h265_chain(),
            warm_video: false,
            pacing: true,
        }
    }

    /// The same options with another audio sink fragment.
    pub fn with_audio_sink(mut self, sink: impl Into<String>) -> Self {
        self.audio_sink = Some(sink.into());
        self
    }

    /// The same options with another video sink fragment.
    pub fn with_video_sink(mut self, sink: impl Into<String>) -> Self {
        self.video_sink = Some(sink.into());
        self
    }

    /// The same options with another chain for one coding.
    pub fn with_video_chain(
        mut self,
        codec: VideoCodec,
        parser: impl Into<String>,
        decoder: impl Into<String>,
    ) -> Self {
        let chain = Chain::new(parser, decoder);
        match codec {
            VideoCodec::H264 => self.h264 = chain,
            VideoCodec::H265 => self.h265 = chain,
            // A coding this backend has no chain for is not a coding it can decode.
            // There is nowhere to put the chain, and nothing to do about it here: the
            // stream that asks for it is refused when it opens (see chain()).
            _ => {}
        }
        self
    }

    /// The same options with another parser for one coding.
    pub fn with_video_parser(self, codec: VideoCodec, parser: impl Into<String>) -> Self {
        let decoder = match self.chain(codec) {
            Ok(chain) => chain.decoder().to_owned(),
            Err(_) => return self,
        };
        self.with_video_chain(codec, parser, decoder)
    }

    /// The same options with another decoder for one coding.
    pub fn with_video_decoder(self, codec: VideoCodec, decoder: impl Into<String>) -> Self {
        let parser = match self.chain(codec) {
            Ok(chain) => chain.parser().to_owned(),
            Err(_) => return self,
        };
        self.with_video_chain(codec, parser, decoder)
    }

    /// The same options, warming a video pipeline between sessions or not.
    pub fn with_warm_video(mut self, warm: bool) -> Self {
        self.warm_video = warm;
        self
    }

    /// The same options, pacing video frames on a cumulative timeline or not.
    ///
    /// Pacing is on by default and is what the core asks for when it does not pace itself:
    /// frames are held to the spacing they arrived at. Turn it off for a sink that already
    /// schedules on its own clock, where a wait here would only add latency to a picture
    /// the sink is about to delay again.
    pub fn with_pacing(mut self, pacing: bool) -> Self {
        self.pacing = pacing;
        self
    }

    /// The same options with in-process pacing turned off.
    ///
    /// Shorthand for `Options::with_pacing(false)`; see there for when that is right.
    pub fn without_pacing(mut self) -> Self {
        self.pacing = false;
        self
    }

    /// The audio sink named, or none when the environment named none.
    pub fn audio_sink(&self) -> Option<&str> {
        self.audio_sink.as_deref()
    }

    /// The video sink named, or none when the environment named none.
    pub fn video_sink(&self) -> Option<&str> {
        self.video_sink.as_deref()
    }

    /// The chain one coding is decoded by.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unavailable`] for a coding this backend does not decode,
    /// which is a machine it cannot ask for the right decoder rather than a broken
    /// request.
    pub fn chain(&self, codec: VideoCodec) -> Result<&Chain, RenderError> {
        match codec {
            VideoCodec::H264 => Ok(&self.h264),
            VideoCodec::H265 => Ok(&self.h265),
            _ => Err(RenderError::Unavailable(format!(
                "this backend does not decode {codec:?}"
            ))),
        }
    }

    /// Whether a video pipeline is kept between sessions.
    pub fn warm_video(&self) -> bool {
        self.warm_video
    }

    /// Whether video frames are paced here.
    pub fn pacing(&self) -> bool {
        self.pacing
    }
}

/// An environment variable's value, when it says something.
fn named(variable: &str) -> Option<String> {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Whether a value of [`WARM_VIDEO_ENV`] asks for a warm pipeline.
///
/// Absent, empty, or one of the usual negatives mean no; anything else means the
/// variable was set, which is what a reader means by setting it.
pub(crate) fn warming_from(value: Option<&str>) -> bool {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    !matches!(
        value.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

#[cfg(test)]
mod tests {
    use super::{Options, warming_from};
    use rust_airplay::VideoCodec;

    #[test]
    fn a_warm_variable_is_read_as_asking_rather_than_naming() {
        assert!(warming_from(Some("1")));
        assert!(warming_from(Some("true")));
        assert!(warming_from(Some("yes")));
        assert!(warming_from(Some("on")));
        assert!(warming_from(Some("anything")));
        assert!(!warming_from(None));
        assert!(!warming_from(Some("")));
        assert!(!warming_from(Some("0")));
        assert!(!warming_from(Some("false")));
        assert!(!warming_from(Some("OFF")));
    }

    #[test]
    fn each_coding_gets_its_own_chain() {
        let options = Options::new("fakesink", "fakesink");
        assert_eq!(
            options.chain(VideoCodec::H264).expect("h264").parser(),
            "h264parse"
        );
        assert_eq!(
            options.chain(VideoCodec::H265).expect("h265").parser(),
            "h265parse"
        );
        let options = options.with_video_parser(VideoCodec::H265, "h265parse");
        assert_eq!(
            options.chain(VideoCodec::H264).expect("h264").parser(),
            "h264parse"
        );
        // The H.265 chain is the one that changed, which is the bug this API exists
        // for: a session that sends H.265 used to be given an H.264 parser.
        assert_eq!(
            options.chain(VideoCodec::H265).expect("h265").parser(),
            "h265parse"
        );
        assert!(!options.warm_video());
    }

    #[test]
    fn pacing_is_on_unless_the_caller_says_otherwise() {
        assert!(Options::default().pacing());
        assert!(Options::new("fakesink", "fakesink").pacing());
        assert!(
            !Options::new("fakesink", "fakesink")
                .without_pacing()
                .pacing()
        );
        assert!(
            !Options::new("fakesink", "fakesink")
                .with_pacing(false)
                .pacing()
        );
        assert!(
            Options::new("fakesink", "fakesink")
                .without_pacing()
                .with_pacing(true)
                .pacing()
        );
    }
}
