// SPDX-License-Identifier: GPL-3.0-or-later
//! The video stream: frames written to a decoder in a subprocess.
//!
//! Video is Annex-B bytes, so it needs no per-frame boundary: a decoder finds the start
//! codes itself. That is what lets video be a separate process, which is the whole point
//! -- a decoder that segfaults on one malformed frame takes only its own process with
//! it. Audio has no such boundary and is linked instead.
//!
//! Nothing here knows about the client's clock. The receiver either paces a mirror itself
//! or pushes at full speed and lets the backend's backpressure decide the rate (core 0.3.1
//! and later); it does not call [GstVideo::set_pace]. The hook stays for a caller that drives
//! the trait directly, and this implementation honours the contract the trait documents: a
//! cumulative timeline (see `crate::pacing`), not a per-frame sleep. A spacing of zero means
//! no wait, and [crate::Options::without_pacing] turns the wait off for a sink that schedules
//! on its own clock.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rust_airplay::{RenderError, VideoFrame, VideoStream};

use crate::options::{Chain, MAX_PACE_SECONDS, MAX_VIDEO_FRAME_BYTES};
use crate::pacing::Pacer;
use crate::pipeline::{self, Pipeline};

/// How much the frame source asks the pipe for at once.
///
/// `fdsrc`'s default block size is a few kilobytes, so a descriptor is read many times per
/// coded frame; naming a larger block lets it hand over a frame at a time. Measured through
/// this backend on 4800 synthetic pictures: 501 ms with the default, 476 ms with 256 KiB,
/// and 423 ms for the same pipeline reading the same stream from a file. Those frames are
/// small, so this measures the pipe rather than a 4K bitrate; a higher bitrate closes the
/// gap by taking the descriptor out of the path, which is what an in-process `appsrc` would
/// do and a subprocess cannot.
const FDSRC_BLOCKSIZE: usize = 256 * 1024;

/// The arguments one video pipeline is started with.
///
/// The frame source is `fdsrc`: frames arrive on standard input, which is the one channel a
/// subprocess has that needs no protocol between the two processes. `sync=false` because a
/// client's clock and a display's are not the same clock, and waiting on one to schedule
/// the other is what makes a mirror show its frames late rather than as they arrive.
pub(crate) fn pipeline_args(chain: &Chain, sink: &[String]) -> Vec<String> {
    let mut args = vec![
        "fdsrc".to_owned(),
        format!("blocksize={FDSRC_BLOCKSIZE}"),
        "!".to_owned(),
    ];
    for fragment in [chain.parser(), "!", chain.decoder(), "!"] {
        for token in fragment.split_whitespace() {
            args.push(token.to_owned());
        }
    }
    args.extend(sink.iter().cloned());
    args.push("sync=false".to_owned());
    args
}

/// The description those arguments came from, for a message that says what was
/// attempted.
pub(crate) fn description(chain: &Chain, sink: &[String]) -> String {
    pipeline_args(chain, sink).join(" ")
}

/// One open video stream.
///
/// It holds the pipeline rather than owning the process, because a warm pipeline is
/// owned by the renderer and survives the stream that used it.
pub(crate) struct GstVideo {
    pipeline: Arc<Pipeline>,
    /// Whether the pipeline outlives this stream.
    warm: bool,
    /// Whether this stream does its own pacing.
    pacing: bool,
    /// Where this stream is on its timeline.
    ///
    /// A mutex because `VideoStream` takes `&self` and pacing is state; the lock is never
    /// contended, because a stream is fed by the thread that owns it.
    pacer: Mutex<Pacer>,
    closed: AtomicBool,
    /// How many frames were accepted, so a stream that showed nothing can be told from
    /// one that was never fed.
    pushed: AtomicU64,
}

impl std::fmt::Debug for GstVideo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GstVideo")
            .field("warm", &self.warm)
            .field("pacing", &self.pacing)
            .field("pushed", &self.pushed.load(Ordering::Relaxed))
            .field("pipeline", &self.pipeline.description())
            .finish_non_exhaustive()
    }
}

impl GstVideo {
    /// Starts a pipeline for this stream, which ends when the stream does.
    ///
    /// # Errors
    ///
    /// Returns what [Pipeline::start] returns, mapped to the render boundary: a missing
    /// element is the machine (unavailable), a broken description is the request
    /// (failed).
    pub(crate) fn start(program: &str, args: &[String], pacing: bool) -> Result<Self, RenderError> {
        let pipeline = Pipeline::start(program, args).map_err(pipeline::as_render_error)?;
        Ok(Self {
            pipeline: Arc::new(pipeline),
            warm: false,
            pacing,
            pacer: Mutex::new(Pacer::default()),
            closed: AtomicBool::new(false),
            pushed: AtomicU64::new(0),
        })
    }

    /// Takes a pipeline the renderer keeps alive between sessions.
    pub(crate) fn adopted(pipeline: Arc<Pipeline>, pacing: bool) -> Self {
        // Said now rather than at the first push: a renderer handing this pipeline to a
        // second session is the thing that must not happen, and it checks this first.
        pipeline.claim();
        Self {
            pipeline,
            warm: true,
            pacing,
            pacer: Mutex::new(Pacer::default()),
            closed: AtomicBool::new(false),
            pushed: AtomicU64::new(0),
        }
    }
}

impl VideoStream for GstVideo {
    fn push(&self, frame: VideoFrame) -> Result<(), RenderError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(RenderError::Failed("the video stream is closed".to_owned()));
        }
        if frame.data.is_empty() {
            // A client can send a frame with no data to mean nothing changed; there is
            // nothing for a decoder to do with it and nothing to say about it.
            return Ok(());
        }
        if frame.data.len() > MAX_VIDEO_FRAME_BYTES {
            return Err(RenderError::Failed(format!(
                "the frame is {} bytes, past the {MAX_VIDEO_FRAME_BYTES} this backend takes",
                frame.data.len()
            )));
        }
        // Moved, not copied: the frame arrives owned and nothing here needs it again.
        self.pipeline
            .push(frame.data)
            .map_err(pipeline::as_render_error)?;
        self.pushed.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Waits until this frame's place on the stream's timeline.
    ///
    /// # Contract
    ///
    /// The receiver does not call this: with core 0.3.1 and later it paces a mirror itself,
    /// or pushes at full speed and lets the pipe push back. A caller that drives the trait
    /// directly gets the wait the trait documents.
    ///
    /// The interval is the spacing the stream has been arriving at, not a request to sleep
    /// that long per frame: the wait is scheduled against a cumulative deadline, so sleep
    /// granularity and per-frame lateness do not accumulate. A stall longer than one
    /// interval re-anchors the timeline instead of being repaid as a burst.
    ///
    /// A value that is zero, negative or not a number means `do not wait`: a caller that
    /// says "no pacing" is not asking for a zero-length sleep either. So does a renderer
    /// built with [crate::Options::without_pacing].
    fn set_pace(&self, interval_seconds: f64) -> Result<(), RenderError> {
        if !self.pacing || !interval_seconds.is_finite() || interval_seconds <= 0.0 {
            return Ok(());
        }
        let interval = Duration::from_secs_f64(interval_seconds.min(MAX_PACE_SECONDS));
        self.pacer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wait(interval);
        Ok(())
    }

    fn flush(&self) -> Result<(), RenderError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(RenderError::Failed("the video stream is closed".to_owned()));
        }
        // A decoder cannot be told to forget what it has queued, and frames that arrive
        // after a flush refer only to the key frame that follows it. Replacing the
        // process is what drops the queue and resets the decoder; a key frame follows a
        // flush, which is what makes the new decoder correct rather than merely empty.
        // The timeline restarts with it: the frames after a flush are not owed the ones
        // that were due before.
        self.pacer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reset();
        self.pipeline.restart().map_err(pipeline::as_render_error)
    }

    fn pause(&self) -> Result<(), RenderError> {
        // A pipeline that stops receiving frames already holds its last one: the sink
        // keeps the picture it was given. There is nothing to freeze here, and stopping
        // the process would cost a restart on resume.
        Ok(())
    }

    fn resume(&self) -> Result<(), RenderError> {
        Ok(())
    }

    fn close(&self) -> Result<(), RenderError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // The session is done writing, whether or not the pipeline is kept.
        self.pipeline.release();
        if self.warm {
            // The pipeline is kept for the next session, which is what warming is for:
            // ending it here would make the next stream as cold as the first one.
            return Ok(());
        }
        // Closing the pipe is the end of stream for a source that reads a descriptor,
        // which is what lets a display show what it has before the process exits.
        self.pipeline.finish().map_err(pipeline::as_render_error)
    }
}

impl Drop for GstVideo {
    fn drop(&mut self) {
        // A stream that was dropped without being closed is still a stream that ended.
        self.pipeline.release();
        // The pipeline it owns stops here; a warm one is the renderer's to stop.
        if !self.warm {
            self.pipeline.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FDSRC_BLOCKSIZE, pipeline_args};
    use crate::options::Chain;

    #[test]
    fn the_pipeline_this_backend_builds_is_the_one_it_documents() {
        let chain = Chain::new("h264parse", "avdec_h264");
        let sink = vec!["fakesink".to_owned()];
        let args = pipeline_args(&chain, &sink);
        assert_eq!(
            args.join(" "),
            format!(
                "fdsrc blocksize={FDSRC_BLOCKSIZE} ! h264parse ! avdec_h264 ! fakesink sync=false"
            )
        );
    }

    #[test]
    fn a_sink_keeps_the_properties_it_was_given() {
        let chain = Chain::new("h264parse", "avdec_h264");
        let sink = vec!["filesink".to_owned(), "location=out.raw".to_owned()];
        let args = pipeline_args(&chain, &sink);
        assert_eq!(
            args.join(" "),
            format!(
                "fdsrc blocksize={FDSRC_BLOCKSIZE} ! h264parse ! avdec_h264 ! filesink location=out.raw sync=false"
            )
        );
    }

    #[test]
    fn a_fragment_chain_is_split_into_the_arguments_gst_launch_needs() {
        let chain = Chain::new("h264parse config-interval=-1", "avdec_h264");
        let sink = vec!["fakesink".to_owned()];
        let args = pipeline_args(&chain, &sink);
        assert_eq!(
            args.join(" "),
            format!(
                "fdsrc blocksize={FDSRC_BLOCKSIZE} ! h264parse config-interval=-1 ! avdec_h264 ! fakesink sync=false"
            )
        );
    }
}
