// SPDX-License-Identifier: GPL-3.0-or-later
//! Feeding a GStreamer pipeline through gst-launch.
//!
//! Video is run as a subprocess rather than linked: the frames arrive as Annex-B bytes,
//! which a byte stream can carry, so a program that reads its standard input is enough,
//! and a decoder that crashes takes only its own process with it. Audio cannot be
//! delivered that way and is linked instead (see the audio module).
//!
//! What happens here is bounded on purpose. A writer thread drains a small queue, so a
//! push gives up rather than blocking a session's receive loop forever, and every wait
//! for the process has a deadline after which the process is killed.

mod error;
mod launch;
mod stderr;
mod writer;

use std::process::Child;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(crate) use error::{LaunchError, as_render_error};
pub(crate) use launch::{LAUNCH_PROGRAM, find_launcher};
use stderr::StderrFile;
use writer::Writer;

/// How long a closed pipeline gets to play out what it has.
///
/// A sink plays its queue before its branch ends, so this is the length of a stream's
/// tail rather than a startup cost. Past it the process is killed: a receiver that
/// hangs on teardown is worse than one that cuts the last frame.
const FINISH_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a killed pipeline gets to be reaped.
const REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a fresh pipeline gets to say it was refused.
///
/// Measured rather than guessed: gst-launch rejects a bad pipeline (a nonexistent
/// element, a sink the machine does not have) after 81 to 91 ms, because what takes
/// the time is the process starting rather than the pipeline being checked. A good
/// one reaches PLAYING in 82 to 85 ms.
const SETTLE: Duration = Duration::from_millis(150);

/// One running pipeline.
///
/// It owns the process, the queue into it and the file its output goes to. The
/// standard input is owned by a writer thread rather than by this struct, so that no
/// lock is held across a write that can block.
#[derive(Debug)]
pub(crate) struct Pipeline {
    program: String,
    args: Vec<String>,
    stderr: StderrFile,
    child: Mutex<Child>,
    writer: Mutex<Option<Writer>>,
    finished: AtomicBool,
    /// Whether a stream is still writing through it.
    ///
    /// A warm pipeline outlives the session that used it, so the stream that is writing
    /// has to say when it has stopped: a second session that adopted it while the first
    /// still pushed would interleave two H.264 streams into one pipe.
    claimed: AtomicBool,
}

impl Pipeline {
    /// Starts a pipeline, with the frames to be written to its standard input.
    ///
    /// The arguments are passed separately rather than as one string, because that is
    /// the form which survives every platform's quoting rules: a caps filter contains
    /// commas, and Windows argument parsing has opinions about commas inside an
    /// argument. gst-launch joins its arguments back together, so the pipeline it
    /// builds is the same one either way.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::StartFailed`] when the program cannot be started, and
    /// [`LaunchError::Rejected`] or [`LaunchError::MissingElement`] when the pipeline it
    /// was given cannot be built.
    pub(crate) fn start(program: &str, args: &[String]) -> Result<Self, LaunchError> {
        let stderr = StderrFile::create()?;
        let (child, writer) = Writer::spawn(program, args, &stderr)?;
        let pipeline = Self {
            program: program.to_owned(),
            args: args.to_vec(),
            stderr,
            child: Mutex::new(child),
            writer: Mutex::new(Some(writer)),
            finished: AtomicBool::new(false),
            claimed: AtomicBool::new(false),
        };
        pipeline.settle();
        if pipeline.is_running() {
            Ok(pipeline)
        } else {
            Err(pipeline.stopped())
        }
    }

    /// The description this pipeline was started from, for a message that says what
    /// was attempted.
    pub(crate) fn description(&self) -> String {
        self.args.join(" ")
    }

    /// Writes one buffer of frames, waiting a bounded time for the pipeline to take it.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::Stopped`] when the pipeline has gone or stopped reading,
    /// with what it said when it said anything.
    pub(crate) fn push(&self, frames: Vec<u8>) -> Result<(), LaunchError> {
        let writer = lock(&self.writer).clone();
        let Some(writer) = writer else {
            return Err(self.stopped());
        };
        writer
            .push(frames)
            .map_err(|reason| LaunchError::Stopped { reason })
    }

    /// Tells the pipeline that no more frames are coming, and waits for it to play what
    /// it has.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::Stopped`] when the pipeline fails or had already gone.
    pub(crate) fn finish(&self) -> Result<(), LaunchError> {
        if self.finished.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // Closing the pipe is the end of stream for a source that reads a descriptor,
        // which is what lets a sink play what it has before it exits.
        if let Some(writer) = lock(&self.writer).take() {
            writer.close();
        }
        self.wait_exit(FINISH_TIMEOUT)
    }

    /// Stops the pipeline now, as a teardown or a flush does.
    pub(crate) fn abort(&self) {
        lock(&self.writer).take();
        let killed = lock(&self.child).kill();
        if let Err(error) = killed {
            log::debug!("{} could not be killed: {error}", self.program);
        }
        let deadline = Instant::now() + REAP_TIMEOUT;
        while Instant::now() < deadline {
            match lock(&self.child).try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => return,
            }
        }
    }

    /// Starts the same pipeline again, after stopping this one.
    ///
    /// This is what a flush costs: a decoder cannot be told to forget what it has
    /// queued, so the process that holds it is replaced. The frames a client sends
    /// after a flush begin with a key frame, which is what makes a fresh decoder
    /// correct rather than merely empty.
    ///
    /// # Errors
    ///
    /// Returns what [`Pipeline::start`] returns.
    pub(crate) fn restart(&self) -> Result<(), LaunchError> {
        self.abort();
        let (child, writer) = Writer::spawn(&self.program, &self.args, &self.stderr)?;
        *lock(&self.child) = child;
        *lock(&self.writer) = Some(writer);
        self.finished.store(false, Ordering::SeqCst);
        self.settle();
        if self.is_running() {
            Ok(())
        } else {
            Err(self.stopped())
        }
    }

    /// Marks the pipeline as being written through by one stream.
    pub(crate) fn claim(&self) {
        self.claimed.store(true, Ordering::SeqCst);
    }

    /// Marks the stream as finished with it, so the next session may adopt it.
    pub(crate) fn release(&self) {
        self.claimed.store(false, Ordering::SeqCst);
    }

    /// Whether a stream is still writing through it.
    pub(crate) fn is_claimed(&self) -> bool {
        self.claimed.load(Ordering::SeqCst)
    }

    /// Whether the pipeline is still running.
    pub(crate) fn is_running(&self) -> bool {
        matches!(lock(&self.child).try_wait(), Ok(None))
    }

    /// Waits, briefly, for a pipeline that was refused to say so.
    fn settle(&self) {
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            if !self.is_running() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits for the process to end, killing it past the deadline.
    fn wait_exit(&self, budget: Duration) -> Result<(), LaunchError> {
        let deadline = Instant::now() + budget;
        loop {
            match lock(&self.child).try_wait() {
                Ok(Some(status)) => {
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(self.stopped_after(status))
                    };
                }
                Ok(None) if Instant::now() >= deadline => {
                    self.abort();
                    return Err(LaunchError::Stopped {
                        reason: format!(
                            "{} did not exit within {}s",
                            self.program,
                            budget.as_secs()
                        ),
                    });
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    return Err(LaunchError::Stopped {
                        reason: error.to_string(),
                    });
                }
            }
        }
    }

    /// What the program said, for a pipeline that has stopped.
    fn stopped(&self) -> LaunchError {
        lock(&self.writer).take();
        let _ = lock(&self.child).kill();
        let _ = lock(&self.child).wait();
        self.said()
    }

    /// What the program said, for a pipeline whose exit status is already known.
    fn stopped_after(&self, status: std::process::ExitStatus) -> LaunchError {
        let reason = self.said();
        match reason {
            LaunchError::Stopped { reason } => LaunchError::Stopped {
                reason: format!("{reason} (exited {status})"),
            },
            other => other,
        }
    }

    /// Classifies a pipeline's output.
    fn said(&self) -> LaunchError {
        let mut reason = self.stderr.read();
        if reason.is_empty() {
            reason = format!("{} exited without saying why", self.program);
        }
        // gst-launch has three ways of saying a description is not one this machine
        // can build, and the caller's answer differs: a missing element is the
        // environment, anything else is the description that was asked for.
        if reason.contains("no element") || reason.contains("No such element") {
            LaunchError::MissingElement { reason }
        } else if reason.contains("erroneous pipeline") || reason.contains("could not link") {
            LaunchError::Rejected { reason }
        } else {
            LaunchError::Stopped { reason }
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // A pipeline left running would hold a window or a sound card for as long as
        // the process lives, and a session that forgot to close one is not a reason to
        // keep playing.
        self.abort();
    }
}

/// Locks a lock this pipeline owns, ignoring poisoning.
///
/// A pipeline whose writer panicked once is still a pipeline, and the alternative --
/// a stream that can never be written to again -- is worse than one written to by
/// whoever is left.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
