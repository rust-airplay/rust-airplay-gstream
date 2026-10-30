// SPDX-License-Identifier: GPL-3.0-or-later
//! One audio stream: an appsrc, and the pipeline whose thread outlives it.
//!
//! The pipeline runs a GLib main loop on a thread of its own, because a bus nobody
//! reads is a failure nobody hears about, and appsrc is only ever as good as the
//! pipeline it feeds. Stopping that thread is Drop's job: a stream that ended without
//! it would leave a sound card open.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSrc;
use rust_airplay::{AudioConfig, AudioFrame, AudioStream, RenderError};

use super::MAX_QUEUED_AUDIO_BYTES;
use super::clock::{elapsed_samples, relative_nanos};
use super::graph::{audio_caps, audio_description};
use super::thread::{Failure, Opened, run};
use crate::pipeline::lock;

/// How long a stream waits for its pipeline to reach end of stream on close.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// How long Drop waits for the pipeline thread to stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// One audio stream, and the pipeline that plays it.
pub(crate) struct GstAudio {
    appsrc: AppSrc,
    /// The volume element in this stream's own pipeline.
    ///
    /// Kept rather than looked up, because a client can change the volume at any time
    /// and set_volume is called from a connection thread while the pipeline plays.
    volume: gst::Element,
    /// The pipeline itself, which a flush drives directly.
    ///
    /// Element state changes are safe from any thread, so a flush does not need to ask
    /// the pipeline thread for anything: it takes the pipeline to READY, which is what
    /// clears every queue, and back to PLAYING.
    pipeline: gst::Pipeline,
    /// What the pipeline said was wrong, if it said anything.
    failure: Failure,
    /// Set when the pipeline has played everything it was given.
    ended: Arc<(Mutex<bool>, Condvar)>,
    /// Stops the thread that runs the pipeline's main loop.
    stop: Mutex<Option<Sender<()>>>,
    /// Fires when that thread has finished, which is what Drop waits for.
    done: Mutex<Option<Receiver<()>>>,
    /// How many frames were pushed, so a silent session can be told from a stalled one.
    pushed: AtomicU64,
    /// The timestamp of the first frame of this stream, which every presentation time
    /// is measured from. Replaced by the next frame after a flush.
    ///
    /// Kept as the client's own 32 bits: an audio timestamp is an RTP counter, and a
    /// receiver that widened it would stop measuring elapsed time once it wrapped,
    /// which a session that runs for a day does.
    first_timestamp: Mutex<Option<u32>>,
    /// The rate a frame's timestamp is counted in, which is what makes it a time.
    sample_rate: u32,
    /// Set by close, so a later flush or push says so rather than pretending.
    closed: AtomicBool,
}

impl std::fmt::Debug for GstAudio {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GstAudio")
            .field("pushed", &self.pushed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl GstAudio {
    /// Builds the pipeline and starts the thread that runs it.
    ///
    /// # Errors
    ///
    /// Returns RenderError::Unavailable when an element is missing or the caps cannot
    /// be described, and RenderError::Failed when the pipeline will not start.
    pub(crate) fn open(config: AudioConfig, sink: &str, volume: f64) -> Result<Self, RenderError> {
        gst::init().map_err(|error| {
            RenderError::Unavailable(format!("GStreamer cannot be initialised: {error}"))
        })?;
        let caps = audio_caps(config)?;
        let description = audio_description(config, sink, volume)?;
        let failure: Failure = Arc::new(Mutex::new(None));
        let watched = Arc::clone(&failure);
        let ended = Arc::new((Mutex::new(false), Condvar::new()));
        let signalled = Arc::clone(&ended);
        let (ready, started) = std::sync::mpsc::channel::<Result<Opened, String>>();
        let (stop, stopping) = std::sync::mpsc::channel::<()>();
        let (finished, done) = std::sync::mpsc::channel::<()>();

        std::thread::Builder::new()
            .name("airplay-gst-audio".to_owned())
            .spawn(move || {
                run(&description, caps, watched, signalled, ready, stopping);
                let _ = finished.send(());
            })
            .map_err(|error| {
                RenderError::Unavailable(format!("the audio thread cannot start: {error}"))
            })?;

        let opened = started
            .recv()
            .map_err(|_| RenderError::Failed("the audio pipeline died at startup".to_owned()))?
            .map_err(RenderError::Unavailable)?;
        let stream = Self {
            appsrc: opened.appsrc,
            volume: opened.volume,
            pipeline: opened.pipeline,
            failure,
            ended,
            stop: Mutex::new(Some(stop)),
            done: Mutex::new(Some(done)),
            pushed: AtomicU64::new(0),
            first_timestamp: Mutex::new(None),
            sample_rate: config.sample_rate.max(1),
            closed: AtomicBool::new(false),
        };
        // A pipeline can fail between the moment it reaches PLAYING and the moment its
        // handle is handed over. Reporting it now means the session says so once, at open,
        // instead of at every frame it pushes into a stream that plays nothing.
        if let Some(reason) = stream.failed() {
            stream.stop_pipeline();
            return Err(RenderError::Failed(reason));
        }
        Ok(stream)
    }

    /// The element a later volume change is applied to.
    pub(crate) fn volume_element(&self) -> gst::Element {
        self.volume.clone()
    }

    /// What the pipeline said was wrong, if anything.
    fn failed(&self) -> Option<String> {
        lock(&self.failure).clone()
    }

    /// Waits for the pipeline thread to stop.
    fn stop_pipeline(&self) {
        if let Some(stop) = lock(&self.stop).take() {
            let _ = stop.send(());
        }
        if let Some(done) = lock(&self.done).take() {
            if done.recv_timeout(STOP_TIMEOUT).is_err() {
                // A pipeline left in PAUSED with a device open is the case stopping is
                // for, so it is forced to NULL from here and given one more moment.
                log::warn!(
                    "the audio pipeline did not stop within {}s; forcing it to NULL",
                    STOP_TIMEOUT.as_secs()
                );
                let _ = self.pipeline.set_state(gst::State::Null);
                if done.recv_timeout(Duration::from_secs(1)).is_err() {
                    log::warn!("the audio pipeline thread is still running");
                }
            }
        }
    }
}

impl AudioStream for GstAudio {
    fn push(&self, frame: AudioFrame) -> Result<(), RenderError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(RenderError::Failed("the audio stream is closed".to_owned()));
        }
        if let Some(reason) = self.failed() {
            // The pipeline started and then failed: that is a request that could not be
            // carried out, not a machine that cannot serve the stream.
            return Err(RenderError::Failed(reason));
        }
        // One buffer, one frame. This is the whole of the delivery rule, and the reason
        // this stream is not a subprocess: a byte stream cannot say where a frame ends,
        // and a decoder given a run of frames reads only the first one.
        //
        // `appsrc` is configured not to block (see the module that builds the pipeline),
        // so a sink that stopped draining would let this queue grow instead of hanging
        // the receive thread; the bound is therefore enforced here.
        let queued = self.appsrc.current_level_bytes();
        if queued.saturating_add(frame.data.len() as u64) > MAX_QUEUED_AUDIO_BYTES {
            return Err(RenderError::Failed(format!(
                "the audio pipeline is not draining: {queued} bytes are already queued"
            )));
        }
        let mut buffer = gst::Buffer::from_slice(frame.data);
        if let Some(buffer) = buffer.get_mut() {
            // Presentation time is measured from the stream's first frame, not from the
            // client's epoch: an audio timestamp is an RTP counter from a random origin,
            // so its absolute value means nothing here. What it does mean is elapsed
            // audio, and a sink told to synchronise schedules against exactly that.
            let stamp = frame.timestamp as u32;
            let first = {
                let mut first = lock(&self.first_timestamp);
                *first.get_or_insert(stamp)
            };
            buffer.set_pts(gst::ClockTime::from_nseconds(relative_nanos(
                elapsed_samples(stamp, first),
                self.sample_rate,
            )));
        }
        self.appsrc.push_buffer(buffer).map_err(|error| {
            RenderError::Failed(format!("the pipeline refused a frame: {error}"))
        })?;
        self.pushed.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn flush(&self) -> Result<(), RenderError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(RenderError::Failed("the audio stream is closed".to_owned()));
        }
        // A flush means what a seek means: what has not been played is dropped, and the
        // decoder forgets the frames it would otherwise refer to. Going to READY is the
        // only thing that does both, and an element's state can be changed from any
        // thread, so this needs nothing from the pipeline thread.
        self.first_timestamp
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.pipeline
            .set_state(gst::State::Ready)
            .map_err(|error| {
                RenderError::Failed(format!("the pipeline will not reset: {error}"))
            })?;
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| {
                RenderError::Failed(format!("the pipeline will not restart: {error}"))
            })?;
        Ok(())
    }

    fn close(&self) -> Result<(), RenderError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // The frames already handed over are played out rather than cut off: an end of
        // stream lets the decoder drain, and waiting for the pipeline to say it reached
        // the end is what makes the sound card's own queue part of the session rather
        // than of whatever plays next.
        self.appsrc
            .end_of_stream()
            .map_err(|error| RenderError::Failed(format!("the stream would not end: {error}")))?;
        if let Some(reason) = self.failed() {
            return Err(RenderError::Failed(reason));
        }
        let (state, condvar) = &*self.ended;
        let mut ended = lock(state);
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        while !*ended {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                log::debug!("the audio stream did not reach end of stream before close");
                break;
            }
            let (next, timeout) = condvar
                .wait_timeout(ended, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ended = next;
            if timeout.timed_out() {
                break;
            }
        }
        Ok(())
    }
}

impl Drop for GstAudio {
    fn drop(&mut self) {
        // The thread owns the pipeline and the sound card it opened, so it is told to
        // stop and waited for: a stream that went away while its pipeline was still
        // playing is a sound card held open by a session that ended.
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.appsrc.end_of_stream();
        }
        self.stop_pipeline();
    }
}
