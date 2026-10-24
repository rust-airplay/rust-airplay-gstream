// SPDX-License-Identifier: GPL-3.0-or-later
//! A renderer that plays through GStreamer.
//!
//! The backend has two renderers, each with a pipeline of its own, and the application chooses
//! the sink.
//!
//! **Audio is linked; video is a subprocess.** That split follows from what the two media
//! allow:
//!
//! - **Video** is run as `gst-launch-1.0` with frames written to its stdin. H.264 and H.265
//!   carry start codes, so a decoder finds its own picture boundaries in a byte stream.
//!   [`pipeline`] says why the subprocess shape was chosen, and `docs/ARCHITECTURE.md` records
//!   the decision.
//! - **Audio** cannot be: a raw AAC-ELD frame has no sync word and no sample rate of its own, so
//!   a byte stream says nothing about where a frame ends. It needs one buffer per frame -
//!   `appsrc` - which is why [`audio`] links `libgstreamer` for it.
//!
//! ```text
//! audio: appsrc ! queue ! avdec_aac ! audioconvert ! audioresample ! <sink>
//! video: fdsrc ! decodebin ! videoconvert ! video/x-raw,format=I420 ! <sink>
//! ```
//!
//! A coding this backend cannot describe is refused when the stream is opened, not when the
//! first frame arrives: a client whose audio never plays should be told once, at the moment a
//! session could still choose differently.

#![deny(missing_docs)]

pub mod audio;
pub mod pipeline;

use std::sync::Mutex;

use rust_airplay::{
    AudioConfig, AudioStream, RenderError, Renderer, VideoCodec, VideoConfig, VideoFrame,
    VideoStream, Volume,
};

use gstreamer::glib::prelude::*;

use pipeline::{LaunchError, Pipeline, find_launcher, lock};

/// The sinks this backend can play into, in the order it prefers them.
///
/// The protocol lets a user choose one (`renderers/`'s sink options) and so does this, by
/// name: a machine whose preferred sink is missing is a machine whose user knows which
/// one it has.
pub const AUDIO_SINKS: &[&str] = &[
    "wasapisink",
    "autoaudiosink",
    "osssink",
    "alsasink",
    "pulsesink",
    "fakesink",
];

/// The video sinks this backend can show, in the order it prefers them.
pub const VIDEO_SINKS: &[&str] = &["d3d11videosink", "autovideosink", "glimagesink", "fakesink"];

/// Environment variable naming the audio sink to use.
pub const AUDIO_SINK_ENV: &str = "AIRPLAY_GST_AUDIO_SINK";

/// Longest a backend waits between two frames.
///
/// A gap longer than this is not a frame that is late, it is a client that stopped
/// and started again — a screen waking up — and a receiver that waited out the gap
/// would appear to hang. One second is longer than any frame interval a client
/// sends.
pub const MAX_PACE_SECONDS: f64 = 1.0;

/// Environment variable naming the video sink to use.
pub const VIDEO_SINK_ENV: &str = "AIRPLAY_GST_VIDEO_SINK";

/// Environment variable that makes the video pipeline start with the receiver.
///
/// Measured, this is most of what a connection waits for. A sink that puts a picture on
/// screen costs, on this machine, 702 ms before it has been given a single frame: the
/// window and the graphics device come up when the pipeline starts, not when the first
/// frame arrives, and a session that starts its pipeline when the client connects pays
/// that while the client is already sending. Starting it at receiver start pays it while
/// nobody is waiting.
///
/// Set to `1` or `true` to turn it on. Off by default for one reason, and it is a real
/// one: a warm pipeline is a window on screen from startup, before anything is being
/// mirrored. A television behaves that way and a desktop application may not.
pub const WARM_VIDEO_ENV: &str = "AIRPLAY_GST_VIDEO_WARM";

/// Everything a caller can choose about how this backend plays.
///
/// The defaults are what the environment variables would produce, so a caller that wants
/// no surprises can take `Options::default` and still override one field.
#[derive(Debug, Clone)]
pub struct Options {
    /// Audio sink name, `wasapisink` on Windows.
    pub audio_sink: String,
    /// Video sink name, `autovideosink`.
    pub video_sink: String,
    /// The parser a mirror pipeline starts with, `h264parse`.
    pub video_parser: String,
    /// The decoder that follows the parser, with any converter it needs.
    pub video_decoder: String,
    /// Whether to start the video pipeline with the receiver rather than the first client.
    pub warm_video: bool,
}

impl Options {
    /// The same options with the video sink replaced, and the chain that sink needs.
    ///
    /// A decoder and a sink are one choice, not two: `d3d11h264dec` decodes into Direct3D11
    /// memory that only `d3d11videosink` can take, and the software decoder needs a
    /// converter in front of every other sink. A caller that changes one without the other
    /// gets the slower path with the faster sink's name on it, which is a mistake this
    /// method exists to make impossible.
    #[must_use]
    pub fn with_video_sink(mut self, sink: &str) -> Self {
        self.video_sink = sink.to_owned();
        let (parser, decoder) = default_video_chain(VideoCodec::H264, sink);
        self.video_parser = parser;
        self.video_decoder = decoder;
        self
    }

    /// The same options with an explicit parser, for a stream this crate does not expect.
    #[must_use]
    pub fn with_video_parser(mut self, parser: &str) -> Self {
        self.video_parser = parser.to_owned();
        self
    }

    /// The same options with an explicit decoder chain.
    #[must_use]
    pub fn with_video_decoder(mut self, decoder: &str) -> Self {
        self.video_decoder = decoder.to_owned();
        self
    }
}

impl Default for Options {
    fn default() -> Self {
        let audio_sink = sink_from_env(AUDIO_SINK_ENV, AUDIO_SINKS, "autoaudiosink");
        let video_sink = sink_from_env(VIDEO_SINK_ENV, VIDEO_SINKS, "autovideosink");
        let (video_parser, video_decoder) = default_video_chain(VideoCodec::H264, &video_sink);
        Self {
            audio_sink,
            video_sink,
            video_parser,
            video_decoder,
            warm_video: warming(),
        }
    }
}

/// A renderer whose streams are GStreamer pipelines.
#[derive(Debug)]
pub struct GstRenderer {
    program: String,
    audio_sink: String,
    video_sink: String,
    volume: Mutex<Volume>,
    /// The parser a mirror pipeline starts with.
    video_parser: String,
    /// The decoder that follows it, with any converter it needs.
    video_decoder: String,

    /// The volume element of the audio stream that is playing, when there is one.
    ///
    /// A client's volume can change mid-session, and a pipeline started before the change
    /// would otherwise keep playing at the volume it opened with.
    live_audio: Mutex<Option<gstreamer::glib::WeakRef<gstreamer::Element>>>,
    /// The video pipeline a session reuses instead of starting its own.
    ///
    /// Held across sessions on purpose: this is the window and the decoder, and dropping
    /// it at the end of a session would mean paying for both again at the next one.
    warm_video: Mutex<Option<(std::sync::Arc<Pipeline>, VideoCodec)>>,
}

impl GstRenderer {
    /// A renderer that will run `gst-launch-1.0`, found on this machine.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unavailable`] when no `gst-launch-1.0` can be run, which
    /// is an environment problem and is reported as one
    /// ([the architecture document](../docs/ARCHITECTURE.md)).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::sync::Arc;
    ///
    /// use rust_airplay::Renderer;
    ///
    /// // Fails unless this machine has GStreamer and `gst-launch-1.0`.
    /// let renderer: Arc<dyn Renderer> =
    /// Arc::new(rust_airplay_gstream::GstRenderer::new.expect("a GStreamer runtime"));
    /// # let _ = renderer;
    /// ```
    pub fn new() -> Result<Self, RenderError> {
        let program = find_launcher().map_err(unavailable)?;
        let video_sink = sink_from_env(VIDEO_SINK_ENV, VIDEO_SINKS, "autovideosink");
        let (video_parser, video_decoder) = default_video_chain(VideoCodec::H264, &video_sink);
        let renderer = Self {
            program,
            audio_sink: sink_from_env(AUDIO_SINK_ENV, AUDIO_SINKS, "autoaudiosink"),
            video_sink,
            video_parser,
            video_decoder,
            volume: Mutex::new(Volume::FULL),
            live_audio: Mutex::new(None),
            warm_video: Mutex::new(None),
        };
        if warming() {
            // A pipeline that cannot start is reported here rather than at the first
            // client: a receiver that will show nothing should say so at startup.
            renderer.warm_video()?;
        }
        Ok(renderer)
    }

    /// A renderer that plays into the sinks named rather than the preferred ones.
    ///
    /// This is how a test checks a pipeline without a sound card or a display, and how a
    /// user whose machine lacks the preferred sink chooses another: `fakesink` accepts
    /// everything and shows nothing, which is a pipeline that can be verified where a
    /// device cannot.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unavailable`] when no `gst-launch-1.0` can be run.
    pub fn with_sinks(audio_sink: &str, video_sink: &str) -> Result<Self, RenderError> {
        let program = find_launcher().map_err(unavailable)?;
        let (video_parser, video_decoder) = default_video_chain(VideoCodec::H264, video_sink);
        Ok(Self {
            program,
            audio_sink: audio_sink.to_owned(),
            video_sink: video_sink.to_owned(),
            video_parser,
            video_decoder,
            volume: Mutex::new(Volume::FULL),
            live_audio: Mutex::new(None),
            warm_video: Mutex::new(None),
        })
    }

    /// Starts the video pipeline now, so that the first client does not wait for it.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unavailable`] when the pipeline cannot be run.
    pub fn warm_video(&self) -> Result<(), RenderError> {
        // A warm pipeline is built for H.264: that is what a client sends unless the
        // receiver advertises more, and a pipeline built for the other coding cannot be
        // borrowed by the first HEVC session - `open_video` checks the coding and starts
        // its own when they differ.
        let description = video_pipeline(&self.video_sink, &self.video_parser, &self.video_decoder);
        let pipeline = Pipeline::start(&self.program, &description).map_err(unavailable)?;
        *lock(&self.warm_video) = Some((std::sync::Arc::new(pipeline), VideoCodec::H264));
        Ok(())
    }

    /// Whether a video pipeline is being kept for the next session.
    #[must_use]
    pub fn video_is_warm(&self) -> bool {
        lock(&self.warm_video).is_some()
    }

    /// The `gst-launch-1.0` this machine will run.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unavailable`] when none can be run.
    pub fn find_launcher() -> Result<String, RenderError> {
        pipeline::find_launcher().map_err(unavailable)
    }

    /// A renderer built from `options` rather than from the environment.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unavailable`] when no `gst-launch-1.0` can be run, which is
    /// an environment problem and is reported as one.
    pub fn with_options(program: &str, options: Options) -> Result<Self, RenderError> {
        let renderer = Self {
            program: program.to_owned(),
            audio_sink: options.audio_sink,
            video_sink: options.video_sink,
            video_parser: options.video_parser,
            video_decoder: options.video_decoder,
            volume: Mutex::new(Volume::FULL),
            live_audio: Mutex::new(None),
            warm_video: Mutex::new(None),
        };
        if options.warm_video {
            renderer.warm_video()?;
        }
        Ok(renderer)
    }

    /// The program this renderer runs pipelines in.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The volume a stream opened later will play at.
    ///
    /// GStreamer's volume is a pipeline element and a pipeline is started before a
    /// client says anything about volume, so a stream reads it when it is opened. What
    /// a client asks for mid-stream reaches a live pipeline in S4-3.
    pub fn volume(&self) -> Volume {
        *self
            .volume
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The audio sink its streams play into.
    pub fn audio_sink(&self) -> &str {
        &self.audio_sink
    }

    /// The video sink its streams show in.
    pub fn video_sink(&self) -> &str {
        &self.video_sink
    }
}

/// Whether the environment asks for a pipeline that starts with the receiver.
fn warming() -> bool {
    matches!(
        std::env::var(WARM_VIDEO_ENV).as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "on")
    )
}

/// Picks a sink: the environment's if it names one, the first available of `preferred`
/// otherwise.
///
/// The list is ordered by preference, and a name early in it may not exist on this machine:
/// `wasapisink` is Windows-only and `d3d11videosink` is Direct3D-only, so taking the first
/// name would make the defaults wrong on every other platform. The element factory answers
/// which names exist, and `fallback` is the name that always works when none of the others do.
fn sink_from_env(variable: &str, preferred: &[&str], fallback: &str) -> String {
    let named = std::env::var(variable).ok();
    choose_sink(named.as_deref(), preferred, fallback)
}

/// The half of [`sink_from_env`] that does not read the environment.
fn choose_sink(named: Option<&str>, preferred: &[&str], fallback: &str) -> String {
    if let Some(name) = named.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    if preferred.is_empty() {
        return fallback.to_owned();
    }
    // Initialising is idempotent and cheap after the first call; a sink cannot be checked
    // without it.
    let _ = gstreamer::init();
    preferred
        .iter()
        .find(|name| gstreamer::ElementFactory::find(name).is_some())
        .map_or_else(|| fallback.to_owned(), |name| (*name).to_owned())
}

/// Turns a pipeline failure into the render error a session reports.
fn unavailable(error: LaunchError) -> RenderError {
    match error {
        LaunchError::NotFound { .. } => RenderError::Unavailable(error.to_string()),
        LaunchError::StartFailed { .. } => RenderError::Unavailable(error.to_string()),
        LaunchError::Rejected { .. } | LaunchError::Stopped { .. } => {
            RenderError::Failed(error.to_string())
        }
    }
}

/// The pipeline one video stream runs.
/// A mirror stream carries H.264 or H.265 depending on what the device sends and
/// what the receiver advertised, and the two need different parsers and decoders.
/// `decodebin` picks them from what arrives, which is what makes one pipeline serve
/// both: a device that switches coding mid-session is then not a special case.
///
/// The format handed to the sink is named rather than left to negotiation. A decoder
/// chooses a format for its own convenience — a hardware one usually produces NV12 —
/// and a sink that is given a format it reads with the wrong stride draws part of the
/// picture correctly and the rest as flat colour, which is a screen that "shows
/// something but not all of it". `videoconvert` to a format every sink reads removes
/// the negotiation from the path a picture takes.
fn video_pipeline(sink: &str, parser: &str, decoder: &str) -> String {
    // The parser and the decoder are named, the way the protocol names them
    // (: `appsrc ! queue ! <parser> !
    // <decoder> ! <converter> ! <sink>`). `decodebin` looks like the general answer and is
    // half the speed of the specific one: measured on a mirror stream of 1554x1080, thirty
    // real frames took 2036 ms through `decodebin` and 1343 ms through `h264parse !
    // avdec_h264`, because `decodebin` runs a typefinder and a dynamic pad for a stream
    // whose format this crate already knows. A client sends about 36 frames a second, so the
    // general pipeline cannot keep up and the queue in front of it drops frames — and a
    // dropped frame is not one lost frame, it is every frame after it that referred to it.
    //
    // H.264 because that is what this crate can read: `CodecConfig` parses an `avcC`, and
    // none of the port handles an `hvcC`.
    format!("fdsrc ! {parser} ! {decoder} ! {sink} sync=false")
}

/// The parser and decoder chain for `sink`, when the caller names neither.
///
/// A client sends H.264, so the parser and decoder for it are named rather than left to
/// `decodebin`. The protocol makes the same two choices configurable (`-vp` for the parser,
/// `-vd` for the decoder, defaulting to `decodebin`), and this crate keeps that shape: the
/// names can be overridden, and the default is the one measured faster here. Thirty real
/// frames of a 1554x1080 stream took 2036 ms through `decodebin` and 1343 ms through
/// `h264parse ! avdec_h264`, because `decodebin` typefinds and links pads for a stream
/// whose format is already known.
fn default_video_chain(codec: VideoCodec, sink: &str) -> (String, String) {
    let parser = match codec {
        VideoCodec::H264 => "h264parse".to_owned(),
        VideoCodec::H265 => "h265parse".to_owned(),
    };
    // The hardware decoders decode into Direct3D11 memory that only the matching sink can
    // take. The software path needs a converter in front of any other sink, and the format
    // is named because a sink given a format it reads with the wrong stride draws part of
    // the picture correctly and the rest as flat colour.
    let (hardware, software) = match codec {
        VideoCodec::H264 => (
            "d3d11h264dec",
            "avdec_h264 ! videoconvert ! video/x-raw,format=I420",
        ),
        VideoCodec::H265 => (
            "d3d11h265dec",
            "avdec_h265 ! videoconvert ! video/x-raw,format=I420",
        ),
    };
    let decoder = if sink == "d3d11videosink" {
        // The registry has to be up before it can be asked what it holds, and this runs
        // while the renderer is being built, before any stream has initialised anything.
        let _ = gstreamer::init();
        if gstreamer::ElementFactory::find(hardware).is_some() {
            hardware.to_owned()
        } else {
            software.to_owned()
        }
    } else {
        software.to_owned()
    };
    (parser, decoder)
}

impl Renderer for GstRenderer {
    fn open_audio(&self, config: AudioConfig) -> Result<Box<dyn AudioStream>, RenderError> {
        let volume = self.volume().fraction();
        let stream = audio::GstAudio::open(config, &self.audio_sink, volume)?;
        // A client can change the volume while this stream plays, so the element is kept
        // where `set_volume` can reach it.
        *lock(&self.live_audio) = Some(stream.volume_element().downgrade());
        Ok(Box::new(stream))
    }

    fn open_video(&self, config: VideoConfig) -> Result<Box<dyn VideoStream>, RenderError> {
        // A warm pipeline is the same window and decoder the last session used. A dead one
        // is not reused: a client that dropped while the sink was being brought up leaves a
        // process that has gone, and the next session deserves a fresh one rather than a
        // pipe nobody reads.
        let mut slot = lock(&self.warm_video);
        if let Some((pipeline, _warm_codec)) = slot
            .as_ref()
            .filter(|(pipeline, codec)| pipeline.is_running() && *codec == config.codec)
        {
            return Ok(Box::new(GstVideo {
                pipeline: std::sync::Arc::clone(pipeline),
                ends_on_close: false,
            }));
        }
        // The coding decides the parser and decoder, and a pipeline built for the other
        // one shows nothing at all: `h264parse` fed an HEVC unit produces no buffers, and
        // nothing in the pipeline reports why.
        let description = match config.codec {
            VideoCodec::H264 => {
                video_pipeline(&self.video_sink, &self.video_parser, &self.video_decoder)
            }
            VideoCodec::H265 => {
                let (parser, decoder) = default_video_chain(VideoCodec::H265, &self.video_sink);
                video_pipeline(&self.video_sink, &parser, &decoder)
            }
        };
        let pipeline = Pipeline::start(&self.program, &description).map_err(unavailable)?;
        let warm = slot.is_some();
        let pipeline = std::sync::Arc::new(pipeline);
        if warm {
            // Warming was asked for and the old pipeline had died, so this one takes its
            // place rather than being dropped at the end of the session.
            *slot = Some((std::sync::Arc::clone(&pipeline), config.codec));
        }
        Ok(Box::new(GstVideo {
            pipeline,
            ends_on_close: !warm,
        }))
    }

    fn set_volume(&self, volume: Volume) -> Result<(), RenderError> {
        // The value is kept for streams opened later - a pipeline starts before a client
        // says anything about volume - and applied to the stream that is playing now. Both
        // matter: a client that turns the volume down expects the sound that is coming out
        // to change, not the next session's.
        *self
            .volume
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = volume;
        let live = lock(&self.live_audio).clone();
        if let Some(element) = live.and_then(|weak| weak.upgrade()) {
            // `volume` is the stream-volume element the audio pipeline is built with; its
            // property is a linear gain, which is what `Volume::fraction` is.
            element.set_property("volume", volume.fraction());
            log::debug!(
                "gstreamer: volume {:.3} applied to the audio stream that is playing",
                volume.fraction()
            );
        } else {
            log::debug!(
                "gstreamer: volume {:.3} kept for the next audio stream; none is playing",
                volume.fraction()
            );
        }
        Ok(())
    }
}

/// One video stream, writing frames to a pipeline.
#[derive(Debug)]
struct GstVideo {
    /// Shared, because a stream is handed to whichever thread plays it and the
    /// pipeline has to outlive the value that opened it.
    pipeline: std::sync::Arc<Pipeline>,
    /// Whether this stream owns the pipeline and may end it.
    ///
    /// A warm pipeline outlives the session that used it, so closing it would close the
    /// window and the next client would pay for a new one — which is the whole thing
    /// warming exists to avoid.
    ends_on_close: bool,
}

impl VideoStream for GstVideo {
    fn push(&self, frame: VideoFrame) -> Result<(), RenderError> {
        self.pipeline.write(&frame.data).map_err(unavailable)
    }

    fn set_pace(&self, until_next_seconds: f64) -> Result<(), RenderError> {
        // GStreamer would do this itself if the sink were allowed to synchronise,
        // but a pipeline fed through a pipe has nothing to synchronise against: the
        // source is a descriptor, and its idea of time is when the bytes arrived.
        // So the caller's clock is applied here, between frames, which is also the
        // shape a future A/V sync needs — one place decides when the next frame is
        // due, and both streams are handed the same answer.
        let seconds = until_next_seconds.clamp(0.0, MAX_PACE_SECONDS);
        if seconds > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), RenderError> {
        Ok(())
    }

    fn close(&self) -> Result<(), RenderError> {
        if self.ends_on_close {
            return self.pipeline.finish().map_err(unavailable);
        }
        // Nothing to end: the pipeline stays up for the next session, which is what makes
        // one start quickly. `/FLUSH` still has somewhere to go — `flush` above — and the
        // decoder resynchronises on the next session's first key frame.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{VIDEO_SINKS, VideoCodec, default_video_chain, video_pipeline};
    use gstreamer::glib::prelude::*;
    use rust_airplay::Renderer as _;

    #[test]
    fn a_named_sink_wins_and_an_empty_preference_falls_back() {
        // A named sink is the caller's decision, even one this machine does not have:
        // failing with the name in the error is more useful than silently using another.
        assert_eq!(
            super::choose_sink(Some("fakesink"), super::VIDEO_SINKS, "autovideosink"),
            "fakesink"
        );
        assert_eq!(
            super::choose_sink(Some("  "), &[], "autovideosink"),
            "autovideosink"
        );
        assert_eq!(
            super::choose_sink(None, &[], "autoaudiosink"),
            "autoaudiosink"
        );
    }

    #[test]
    #[ignore = "needs a GStreamer runtime; see README.md"]
    fn the_default_sinks_exist_on_this_machine() {
        // The bug this pins: the defaults took the first *name* in the preference list, so
        // on every platform that is not Windows the audio default was `wasapisink` and the
        // video default `d3d11videosink`, neither of which exists there.
        let _ = gstreamer::init();
        for sink in [super::AUDIO_SINKS, super::VIDEO_SINKS] {
            let chosen = super::choose_sink(None, sink, "fakesink");
            assert!(
                gstreamer::ElementFactory::find(&chosen).is_some(),
                "{chosen} was chosen but does not exist"
            );
        }
    }

    #[test]
    #[ignore = "needs a GStreamer runtime; see README.md"]
    fn a_volume_change_reaches_the_audio_stream_that_is_playing() {
        // The bug this pins: `set_volume` only remembered the value for the next stream,
        // so turning the volume down on the sending device changed nothing that could be
        // heard.
        let Ok(renderer) = super::GstRenderer::with_sinks("fakesink", "fakesink") else {
            eprintln!("skipping: no GStreamer runtime");
            return;
        };
        let stream = renderer
            .open_audio(rust_airplay::AudioConfig::default())
            .expect("an audio stream");
        renderer
            .set_volume(rust_airplay::Volume::new(0.25))
            .expect("a volume");
        let element = {
            let live = super::lock(&renderer.live_audio);
            live.as_ref().and_then(|weak| weak.upgrade())
        }
        .expect("the stream that is playing");
        let applied: f64 = element.property("volume");
        assert!(
            (applied - 0.25).abs() < 1e-9,
            "the element plays at {applied}"
        );
        drop(stream);
    }

    #[test]
    fn changing_the_sink_changes_the_decoder_with_it() {
        // The bug this pins: the command line replaced the sink and left the decoder at
        // what the default sink had chosen, so the hardware sink was fed by the software
        // decoder through a converter - the slower path wearing the faster one's name.
        let options = super::Options::default().with_video_sink("fakesink");
        let options_hardware = options.clone().with_video_sink("d3d11videosink");
        assert_eq!(options_hardware.video_sink, "d3d11videosink");
        // On a machine with the hardware decoder, the chain is the hardware one; without
        // it, the software one is still valid. Either way the converter is only there when
        // the decoder needs it.
        if gstreamer::ElementFactory::find("d3d11h264dec").is_some() {
            assert_eq!(options_hardware.video_decoder, "d3d11h264dec");
        } else {
            assert!(options_hardware.video_decoder.contains("avdec_h264"));
        }
        // And going back to a sink that cannot take hardware memory restores the converter.
        let back = options_hardware.with_video_sink("fakesink");
        assert!(
            back.video_decoder.contains("avdec_h264"),
            "{}",
            back.video_decoder
        );
        assert!(
            back.video_decoder.contains("videoconvert"),
            "{}",
            back.video_decoder
        );
    }
    #[test]
    fn a_sink_that_cannot_take_hardware_memory_gets_the_software_decoder() {
        // `d3d11h264dec` decodes into Direct3D11 memory, which nothing else can consume.
        // Handing it to another sink would fail where a client connects rather than here.
        let (_, chain) = default_video_chain(VideoCodec::H264, "fakesink");
        assert!(chain.contains("avdec_h264"), "{chain}");
        assert!(chain.contains("videoconvert"), "{chain}");
        assert!(!chain.contains("d3d11h264dec"), "{chain}");
    }

    #[test]
    fn every_pipeline_names_a_parser_and_a_decoder() {
        // Not `decodebin`: measured at half the throughput of the named pair (see
        // `video_pipeline`), and the protocol does not use it either.
        for sink in VIDEO_SINKS {
            let (parser, decoder) = default_video_chain(VideoCodec::H264, sink);
            let pipeline = video_pipeline(sink, &parser, &decoder);
            assert!(pipeline.contains("h264parse"), "{pipeline}");
            assert!(
                pipeline.contains("d3d11h264dec") || pipeline.contains("avdec_h264"),
                "{pipeline}"
            );
            assert!(pipeline.contains(&parser), "{pipeline}");
        }
    }

    #[test]
    fn an_hevc_session_gets_an_hevc_parser_and_decoder() {
        // `h264parse` fed an HEVC unit produces nothing at all, and nothing in the
        // pipeline says why - so the coding has to choose the chain, not the sink alone.
        let (parser, decoder) = default_video_chain(VideoCodec::H265, "fakesink");
        assert_eq!(parser, "h265parse");
        assert!(decoder.contains("avdec_h265"), "{decoder}");
        // The hardware decoder is only chosen when the sink can take its memory.
        let (_, hardware) = default_video_chain(VideoCodec::H265, "d3d11videosink");
        assert!(
            hardware.contains("d3d11h265dec") || hardware.contains("avdec_h265"),
            "{hardware}"
        );
    }
}
