// SPDX-License-Identifier: GPL-3.0-or-later
//! The pipe into a pipeline's standard input.
//!
//! A pipeline reads its frames from a descriptor, and writing to that descriptor can
//! block for as long as the pipeline declines to read. The writing therefore happens on
//! a thread of its own, behind a queue a few buffers deep: a push hands one buffer over
//! and waits for that buffer to be taken, so a decoder that has stopped reading becomes
//! a refused frame rather than a session that never returns.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::time::Duration;

use super::LaunchError;
use super::stderr::StderrFile;

/// How long a push waits for the writer to take one buffer.
///
/// A pipeline that has stopped reading is not one that will start again: past this the
/// frame is refused and the stream reports why, rather than holding the session that
/// sent it.
pub(super) const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// How many buffers the queue may hold.
///
/// A few rather than unbounded: a pipeline that stops reading must not let a session
/// accumulate its whole stream in memory.
const QUEUE: usize = 4;

/// How long closing the pipe waits for the writer to get to it.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// One buffer for a pipeline, and the answer to whether it was taken.
enum Job {
    Data(Vec<u8>, Sender<Result<(), String>>),
    Close(Sender<()>),
}

/// The queue into a pipeline's standard input.
#[derive(Clone, Debug)]
pub(super) struct Writer {
    jobs: SyncSender<Job>,
}

impl Writer {
    /// Starts the program and the thread that writes to it.
    ///
    /// The arguments are passed separately rather than as one string, because that is
    /// the form which survives every platform's quoting rules: a caps filter contains
    /// commas, and Windows argument parsing has opinions about commas inside an
    /// argument. gst-launch joins its arguments back together, so the pipeline it builds
    /// is the same one either way.
    pub(super) fn spawn(
        program: &str,
        args: &[String],
        stderr: &StderrFile,
    ) -> Result<(Child, Self), LaunchError> {
        let mut child = Command::new(program)
            .arg("-q")
            .arg("-e")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr.open()?))
            .spawn()
            .map_err(|error| LaunchError::StartFailed {
                reason: error.to_string(),
            })?;
        let Some(stdin) = child.stdin.take() else {
            return Err(LaunchError::StartFailed {
                reason: "the pipeline has no standard input".to_owned(),
            });
        };
        let (jobs, receiving) = std::sync::mpsc::sync_channel::<Job>(QUEUE);
        std::thread::Builder::new()
            .name("airplay-gst-writer".to_owned())
            .spawn(move || write_loop(stdin, receiving))
            .map_err(|error| LaunchError::StartFailed {
                reason: format!("the pipeline writer cannot start: {error}"),
            })?;
        Ok((child, Self { jobs }))
    }

    /// Writes one buffer of frames, waiting at most [`WRITE_TIMEOUT`] for the pipeline
    /// to take it.
    ///
    /// The error is what the pipeline said, or why the buffer was not taken in time.
    pub(super) fn push(&self, frames: &[u8]) -> Result<(), String> {
        let (acknowledge, answer) = std::sync::mpsc::channel();
        if self
            .jobs
            .send(Job::Data(frames.to_vec(), acknowledge))
            .is_err()
        {
            return Err("the pipeline is no longer reading".to_owned());
        }
        match answer.recv_timeout(WRITE_TIMEOUT) {
            Ok(outcome) => outcome,
            Err(_) => Err(format!(
                "the pipeline did not take a frame within {}s",
                WRITE_TIMEOUT.as_secs()
            )),
        }
    }

    /// Closes the pipe, which is the end of stream for a source that reads a descriptor.
    pub(super) fn close(&self) {
        let (closed, closed_rx) = std::sync::mpsc::channel();
        if self.jobs.send(Job::Close(closed)).is_ok() {
            let _ = closed_rx.recv_timeout(CLOSE_TIMEOUT);
        }
    }
}

/// Writes what the queue holds, until the queue ends or a write fails.
fn write_loop(mut stdin: ChildStdin, jobs: Receiver<Job>) {
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Data(frames, acknowledge) => {
                let outcome = stdin.write_all(&frames).map_err(|error| error.to_string());
                let failed = outcome.is_err();
                let _ = acknowledge.send(outcome);
                if failed {
                    return;
                }
            }
            Job::Close(closed) => {
                drop(stdin);
                let _ = closed.send(());
                return;
            }
        }
    }
}
