// SPDX-License-Identifier: GPL-3.0-or-later
//! The renderer the core hands a session.

use std::sync::{Arc, Mutex};

use gst::glib::WeakRef;
use gst::glib::prelude::*;
use gstreamer as gst;
use rust_airplay::{
    AudioConfig, AudioStream, RenderError, Renderer, VideoCodec, VideoConfig, VideoStream, Volume,
};

use crate::audio::GstAudio;
use crate::options::{AUDIO_SINKS, Options, VIDEO_SINKS, resolve_sink};
use crate::pipeline::{self, LAUNCH_PROGRAM, Pipeline, find_launcher, lock};
use crate::video::{self, GstVideo};

/// A renderer that plays through GStreamer.
///
/// One renderer serves one session's audio and video at once, which is why every field
/// that changes is behind a lock: the core calls it from the thread that received a
/// control message while the media threads are pushing frames through the streams it
/// opened.
///
/// A machine with no usable sink or no GStreamer at all is refused when the renderer is
/// built rather than when a frame arrives, so that a program can report what is wrong
/// once, at startup, and choose another backend. A machine with no
pub struct GstRenderer {
    /// The gst-launch program found at startup, if there was one.
    ///
    /// Absent is not fatal: audio does not need it, so a machine that plays audio and
    /// cannot decode video still has a renderer.
    program: Option<String>,
    /// What the caller asked for, which decides the chains and whether to warm.
    options: Options,
    /// The audio sink as a description, with whatever properties the caller gave it.
    audio_sink: String,
    /// The video sink, in the same two forms.
    video_sink: String,
    video_sink_args: Vec<String>,
    /// The volume a stream that opens later plays at.
    volume: Mutex<Volume>,
    /// The volume element of the audio stream that is open, if one is.
    ///
    /// A weak reference because the stream owns its pipeline: keeping the element alive
    /// here would keep a finished stream's pipeline playing.
    live_audio: Mutex<Option<WeakRef<gst::Element>>>,
    /// The video pipeline kept between sessions, and the coding it was built for.
    warm: Mutex<Option<(Arc<Pipeline>, VideoCodec)>>,
}

impl std::fmt::Debug for GstRenderer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GstRenderer")
            .field("program", &self.program)
            .field("audio_sink", &self.audio_sink)
            .field("video_sink", &self.video_sink)
            .field("volume", &lock(&self.volume))
            .field("video_is_warm", &self.video_is_warm())
            .finish_non_exhaustive()
    }
}

impl GstRenderer {
    /// Builds a renderer from the environment and the sinks this backend prefers.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unavailable`] when no sink this backend knows is
    /// installed, or when warming was asked for and there is no gst-launch to warm.
    pub fn new() -> Result<Self, RenderError> {
        Self::with_options(Options::default())
    }

    /// Builds a renderer that plays into these sinks.
    ///
    /// Each argument is a sink and, optionally, its properties -- for example
    /// [AUDIO_SINKS]
    /// vs a filesink with a location. The element must be installed; the properties are
    /// passed through.
    ///
    /// # Errors
    ///
    /// The same as [GstRenderer::new].
    pub fn with_sinks(audio_sink: &str, video_sink: &str) -> Result<Self, RenderError> {
        Self::with_options(Options::new(audio_sink, video_sink))
    }

    /// Builds a renderer that plays into these sinks, with whatever chains they name.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unavailable`] when a named sink is not installed or the
    /// GStreamer registry cannot be initialised, and
    /// [`RenderError::Failed`] when a sink description is not one: a fragment that
    /// names an element and its properties, rather than a pipeline of its own.
    pub fn with_options(options: Options) -> Result<Self, RenderError> {
        gst::init().map_err(|error| {
            RenderError::Unavailable(format!("GStreamer cannot be initialised: {error}"))
        })?;
        let audio_sink_args = resolve_sink(options.audio_sink(), AUDIO_SINKS, "audio")?;
        let video_sink_args = resolve_sink(options.video_sink(), VIDEO_SINKS, "video")?;
        let program = match find_launcher() {
            Ok(program) => Some(program),
            Err(error) => {
                if options.warm_video() {
                    // Warming is a request for a pipeline kept between sessions, and
                    // there is nothing to keep one in without the program that runs it.
                    return Err(pipeline::as_render_error(error));
                }
                // Everything but video works without it, and a backend that refused to
                // exist would take the audio with it.
                log::warn!("{error}; video will be unavailable");
                None
            }
        };
        Ok(Self {
            program,
            audio_sink: audio_sink_args.join(" "),
            video_sink: video_sink_args.join(" "),
            video_sink_args,
            volume: Mutex::new(Volume::FULL),
            live_audio: Mutex::new(None),
            warm: Mutex::new(None),
            options,
        })
    }

    /// The gst-launch program in use, if one was found.
    pub fn program(&self) -> Option<&str> {
        self.program.as_deref()
    }

    /// Finds gst-launch now, which is what a program does when it wants to report
    /// whether this machine can decode video.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unavailable`] when no program was found.
    pub fn find_launcher(&self) -> Result<String, RenderError> {
        find_launcher().map_err(pipeline::as_render_error)
    }

    /// The volume every stream plays at.
    pub fn volume(&self) -> Volume {
        *lock(&self.volume)
    }

    /// The audio sink in use.
    pub fn audio_sink(&self) -> &str {
        &self.audio_sink
    }

    /// The video sink in use.
    pub fn video_sink(&self) -> &str {
        &self.video_sink
    }

    /// Whether a video pipeline is being kept for the next session.
    pub fn video_is_warm(&self) -> bool {
        lock(&self.warm)
            .as_ref()
            .is_some_and(|(pipeline, _)| pipeline.is_running())
    }

    /// What the video pipeline for a stream will be, which is what an error quotes.
    pub fn video_description(&self, codec: VideoCodec) -> String {
        match self.options.chain(codec) {
            Ok(chain) => video::description(chain, &self.video_sink_args),
            Err(error) => error.to_string(),
        }
    }
}

impl Renderer for GstRenderer {
    fn open_audio(&self, config: AudioConfig) -> Result<Box<dyn AudioStream>, RenderError> {
        let stream = GstAudio::open(config, &self.audio_sink, lock(&self.volume).level())?;
        // The element rather than a copy of the volume: a later set_volume reaches the
        // pipeline that is playing, not only the next one.
        let element = stream.volume_element();
        let weak = WeakRef::new();
        weak.set(Some(&element));
        *lock(&self.live_audio) = Some(weak);
        Ok(Box::new(stream))
    }

    fn open_video(&self, config: VideoConfig) -> Result<Box<dyn VideoStream>, RenderError> {
        let chain = self.options.chain(config.codec)?;
        let description = video::description(chain, &self.video_sink_args);
        if !self.options.warm_video() {
            let program = self.program.as_deref().ok_or_else(|| {
                RenderError::Unavailable(format!(
                    "{LAUNCH_PROGRAM} was not found, so no video can be decoded; the pipeline would have been {description}"
                ))
            })?;
            let args = video::pipeline_args(chain, &self.video_sink_args);
            return Ok(Box::new(GstVideo::start(program, &args)?));
        }
        // The kept pipeline stays in the slot while a session uses it: the stream holds a
        // reference, the slot holds another, and the one thing that must not happen is
        // the pipeline being dropped -- and with it the decoder, and with that the cost
        // warming exists to avoid -- the moment a session ends.
        let mut kept = lock(&self.warm);
        if let Some((pipeline, codec)) = kept.as_ref() {
            if *codec == config.codec && pipeline.is_running() {
                // The pipeline a previous session left behind, still decoding and still
                // showing what it was told to last.
                return Ok(Box::new(GstVideo::adopted(Arc::clone(pipeline))));
            }
        }
        // A pipeline for another coding, or one that stopped while it waited.
        if let Some((pipeline, _)) = kept.take() {
            pipeline.abort();
        }
        let program = self.program.as_deref().ok_or_else(|| {
            RenderError::Unavailable(format!(
                "{LAUNCH_PROGRAM} was not found, so no video can be decoded; the pipeline would have been {description}"
            ))
        })?;
        let args = video::pipeline_args(chain, &self.video_sink_args);
        let pipeline =
            Arc::new(Pipeline::start(program, &args).map_err(pipeline::as_render_error)?);
        *kept = Some((Arc::clone(&pipeline), config.codec));
        Ok(Box::new(GstVideo::adopted(pipeline)))
    }

    fn set_volume(&self, volume: Volume) -> Result<(), RenderError> {
        *lock(&self.volume) = volume;
        let live = lock(&self.live_audio).clone();
        if let Some(element) = live.and_then(|weak| weak.upgrade()) {
            // The element scales linearly, which is what the volume type's level is;
            // its curve is applied by the caller that chose it.
            element.set_property("volume", volume.level());
        }
        Ok(())
    }
}

impl Drop for GstRenderer {
    fn drop(&mut self) {
        // A warmed pipeline is kept across sessions but is not meant to outlive the
        // renderer: nothing would be left to use it.
        if let Some((pipeline, _)) = lock(&self.warm).take() {
            pipeline.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::GstRenderer;
    use crate::options::Options;
    use rust_airplay::{RenderError, Renderer, Volume};

    #[test]
    fn a_renderer_reports_the_sinks_it_was_given() {
        gstreamer::init().expect("gstreamer");
        let renderer = GstRenderer::with_sinks("fakesink", "fakesink").expect("a renderer");
        assert_eq!(renderer.audio_sink(), "fakesink");
        assert_eq!(renderer.video_sink(), "fakesink");
        assert!(!renderer.video_is_warm());
    }

    #[test]
    fn a_sink_that_is_not_installed_is_refused_when_the_renderer_is_built() {
        gstreamer::init().expect("gstreamer");
        let error = GstRenderer::with_sinks("no-such-sink-name", "fakesink")
            .expect_err("a missing element is not a renderer");
        assert!(matches!(error, RenderError::Unavailable(_)), "{error}");
        assert!(error.to_string().contains("no-such-sink-name"), "{error}");
    }

    #[test]
    fn a_description_that_is_a_pipeline_is_refused() {
        gstreamer::init().expect("gstreamer");
        let error = GstRenderer::with_sinks("fakesink ! filesink", "fakesink")
            .expect_err("a pipeline is not a sink");
        assert!(matches!(error, RenderError::Failed(_)), "{error}");
    }

    #[test]
    fn the_volume_outlives_the_stream_it_was_set_on() {
        gstreamer::init().expect("gstreamer");
        let renderer =
            GstRenderer::with_options(Options::new("fakesink", "fakesink").with_warm_video(false))
                .expect("a renderer");
        renderer
            .set_volume(Volume::new(0.25))
            .expect("a renderer without a stream takes a volume");
        assert_eq!(renderer.volume().level(), 0.25);
    }
}
