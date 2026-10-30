// SPDX-License-Identifier: GPL-3.0-or-later
//! The pipe into a pipeline's standard input.
//!
//! A pipeline reads its frames from a descriptor, and writing to that descriptor can
//! block for as long as the pipeline declines to read. The writing therefore happens on
//! a thread of its own, behind a queue a few buffers deep: a push hands one buffer over
//! and waits for that buffer to be taken, so a decoder that has stopped reading becomes
//! a refused frame rather than a session that never returns.
//!
//! Every wait here has a deadline, including the wait for room in the queue: a full queue
//! means the writer thread is stuck in a write, so offering it another job is the same
//! wait as the write itself, and it has to give up the same way.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError};
use std::time::{Duration, Instant};

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
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaunchError::StartFailed {
                reason: "the pipeline has no standard input".to_owned(),
            });
        };
        let (jobs, receiving) = std::sync::mpsc::sync_channel::<Job>(QUEUE);
        if let Err(error) = std::thread::Builder::new()
            .name("airplay-gst-writer".to_owned())
            .spawn(move || write_loop(stdin, receiving))
        {
            // The child exists whether or not the thread does, and a pipeline with no
            // writer is a process nobody will ever feed or stop.
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaunchError::StartFailed {
                reason: format!("the pipeline writer cannot start: {error}"),
            });
        }
        Ok((child, Self { jobs }))
    }

    /// Writes one buffer of frames, waiting at most [`WRITE_TIMEOUT`] for the pipeline
    /// to take it.
    ///
    /// The buffer is moved rather than copied: a 1080p frame is tens of kilobytes and a
    /// session is tens of frames a second, which is worth not copying twice.
    ///
    /// The error is what the pipeline said, or why the buffer was not taken in time.
    pub(super) fn push(&self, frames: Vec<u8>) -> Result<(), String> {
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let (acknowledge, answer) = std::sync::mpsc::channel();
        enqueue(&self.jobs, Job::Data(frames, acknowledge), deadline)?;
        let left = deadline.saturating_duration_since(Instant::now());
        match answer.recv_timeout(left) {
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
        let deadline = Instant::now() + CLOSE_TIMEOUT;
        if enqueue(&self.jobs, Job::Close(closed), deadline).is_ok() {
            let _ = closed_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
    }
}

/// Hands a job to the writer thread, waiting at most until `deadline` for room in the
/// queue.
///
/// This is the part that has to be bounded: `SyncSender::send` waits as long as the writer
/// keeps the queue full, which is exactly as long as the pipeline refuses to read, which is
/// the case this module exists to survive.
fn enqueue(jobs: &SyncSender<Job>, mut job: Job, deadline: Instant) -> Result<(), String> {
    loop {
        match jobs.try_send(job) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                job = returned;
                if Instant::now() >= deadline {
                    return Err("the pipeline is not taking frames: its writer is still"
                        .to_owned()
                        + " queued behind a write");
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err("the pipeline is no longer reading".to_owned());
            }
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

#[cfg(test)]
mod tests {
    use super::{Job, Writer, enqueue};
    use std::time::{Duration, Instant};

    #[test]
    fn a_full_queue_is_refused_rather_than_waited_out() {
        // A writer that has not taken a single job is a pipeline that is not reading: the
        // second job must give up by its deadline instead of blocking the caller for as
        // long as the pipeline stays stuck.
        let (jobs, _receiving) = std::sync::mpsc::sync_channel::<Job>(1);
        let writer = Writer { jobs };
        let (acknowledge, _answer) = std::sync::mpsc::channel();
        enqueue(
            &writer.jobs,
            Job::Data(vec![1, 2, 3], acknowledge),
            Instant::now() + Duration::from_secs(1),
        )
        .expect("the first job fits");
        let (acknowledge, _answer) = std::sync::mpsc::channel();
        let started = Instant::now();
        let error = enqueue(
            &writer.jobs,
            Job::Data(vec![4, 5, 6], acknowledge),
            started + Duration::from_millis(60),
        )
        .expect_err("a full queue is an error");
        assert!(started.elapsed() < Duration::from_secs(2), "{error}");
        assert!(error.contains("not taking frames"), "{error}");
    }

    #[test]
    fn a_queue_with_no_writer_says_so() {
        let (jobs, receiving) = std::sync::mpsc::sync_channel::<Job>(1);
        drop(receiving);
        let (acknowledge, _answer) = std::sync::mpsc::channel();
        let error = enqueue(
            &jobs,
            Job::Data(Vec::new(), acknowledge),
            Instant::now() + Duration::from_millis(50),
        )
        .expect_err("a dropped writer is an error");
        assert!(error.contains("no longer reading"), "{error}");
    }
}
