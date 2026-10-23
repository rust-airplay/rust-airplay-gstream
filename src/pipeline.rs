// SPDX-License-Identifier: GPL-3.0-or-later
//! Feeding a GStreamer pipeline through `gst-launch`.
//!
//! The backend drives GStreamer as a subprocess rather than linking it, and this module
//! is the whole of that: it finds `gst-launch-1.0`, starts it with a pipeline, writes
//! frames to its standard input, and reports what it said when it fails. The reasoning
//! behind the choice is in [`docs/ARCHITECTURE.md`]; what matters here is that
//! nothing in it is unsafe and nothing in it needs a compiler for a C library.
//!
//! [`docs/ARCHITECTURE.md`]: ../../../docs/ARCHITECTURE.md

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;

/// The program a pipeline runs in.
pub const LAUNCH_PROGRAM: &str = "gst-launch-1.0";

/// Names to try for [`LAUNCH_PROGRAM`], in order, for a machine that has it under another
/// one.
pub const LAUNCH_NAMES: &[&str] = &["gst-launch-1.0", "gst-launch-1.0.exe"];

/// Environment variable naming the program to run, for a runtime this backend would not
/// find on its own.
pub const LAUNCH_ENV: &str = "AIRPLAY_GST_LAUNCH";

/// Why a pipeline could not be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// No `gst-launch-1.0` was found, with the names tried.
    NotFound {
        /// The names tried, in order.
        tried: Vec<String>,
    },
    /// The program could not be started.
    StartFailed {
        /// What the operating system said.
        reason: String,
    },
    /// The pipeline description was refused before it ran.
    Rejected {
        /// What the program said about it.
        reason: String,
    },
    /// The pipeline stopped while it was running.
    Stopped {
        /// What the program said about it.
        reason: String,
    },
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { tried } => write!(
                formatter,
                "no {LAUNCH_PROGRAM} was found (looked for {})",
                tried.join(", ")
            ),
            Self::StartFailed { reason } => {
                write!(formatter, "cannot start {LAUNCH_PROGRAM}: {reason}")
            }
            Self::Rejected { reason } => write!(formatter, "the pipeline was refused: {reason}"),
            Self::Stopped { reason } => write!(formatter, "the pipeline stopped: {reason}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// The program to run, or the one [`LAUNCH_ENV`] names.
///
/// # Errors
///
/// Returns [`LaunchError::NotFound`] when no name can be run. Finding one is a search of
/// `PATH` by the operating system when the process starts, so this asks each candidate for
/// its version once: a name that answers is a name that works, and one that does not is
/// passed over rather than reported as the reason a session has no audio.
pub fn find_launcher() -> Result<String, LaunchError> {
    let named = std::env::var(LAUNCH_ENV).ok();
    let names: Vec<String> = match &named {
        Some(name) => vec![name.clone()],
        None => LAUNCH_NAMES.iter().map(|name| (*name).to_owned()).collect(),
    };
    for name in &names {
        let started = Command::new(name)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if started.is_ok_and(|status| status.success()) {
            return Ok(name.clone());
        }
    }
    Err(LaunchError::NotFound { tried: names })
}

/// One running pipeline.
///
/// It holds the program and the pipe into it, behind a lock because a pipe has one writer
/// and a session's media can arrive on more than one thread. What the program says goes to
/// a file rather than into this process, for the reason the function that opens it gives.
#[derive(Debug)]
pub struct Pipeline {
    program: String,
    stderr: std::path::PathBuf,
    child: Mutex<Child>,
    input: Mutex<Option<ChildStdin>>,
}

impl Pipeline {
    /// Starts a pipeline description, with the frames to be written to its standard input.
    ///
    /// The description is passed as separate arguments rather than as one string, because
    /// that is the form which survives every platform's quoting rules: a caps filter
    /// contains commas, and Windows argument parsing has opinions about commas inside an
    /// argument. `gst-launch` joins its arguments back together, so the pipeline it builds
    /// is the same one either way.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::StartFailed`] when the program cannot be started, and
    /// [`LaunchError::Rejected`] when the pipeline it was given does not exist.
    pub fn start(program: &str, description: &str) -> Result<Self, LaunchError> {
        let (stderr_path, stderr) = open_stderr()?;
        let mut child = Command::new(program)
            .arg("-q")
            .arg("-e")
            .args(description.split_whitespace())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .map_err(|error| LaunchError::StartFailed {
                reason: error.to_string(),
            })?;
        let input = child.stdin.take();
        let pipe = Self {
            program: program.to_owned(),
            stderr: stderr_path,
            child: Mutex::new(child),
            input: Mutex::new(input),
        };
        // `gst-launch` builds its pipeline and only then says what it thinks of it, so a
        // pipeline it cannot build has already gone by the time this returns. A session
        // that opened a stream it cannot use should hear about it now rather than at the
        // first frame; a pipeline it accepts is waiting for frames on the pipe, which is
        // what the pipe is for.
        pipe.settle();
        if pipe.is_running() {
            Ok(pipe)
        } else {
            Err(pipe.stopped())
        }
    }

    /// The program this pipeline runs in.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// The file this pipeline's output goes to, for a caller that logs where to look.
    pub fn stderr_path(&self) -> &std::path::Path {
        &self.stderr
    }

    /// Writes one buffer of frames.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::Stopped`] when the pipeline has gone, with what it said. A
    /// pipeline that refuses a frame is not something to keep writing to: a decoder that
    /// cannot read what it is given will not start reading it later.
    pub fn write(&self, frames: &[u8]) -> Result<(), LaunchError> {
        let mut input = lock(&self.input);
        let Some(stdin) = input.as_mut() else {
            return Err(self.stopped());
        };
        match stdin.write_all(frames) {
            Ok(()) => Ok(()),
            Err(_) => Err(self.stopped()),
        }
    }

    /// Tells the pipeline that no more frames are coming, and waits for it to play what it
    /// has.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::Stopped`] when the pipeline fails or has already gone.
    pub fn finish(&self) -> Result<(), LaunchError> {
        // Closing the pipe is the end of stream for a source that reads a descriptor,
        // which is what lets a sink play what it has before it exits.
        lock(&self.input).take();
        let outcome = lock(&self.child)
            .wait()
            .map_err(|error| LaunchError::Stopped {
                reason: error.to_string(),
            })?;
        if outcome.success() {
            Ok(())
        } else {
            Err(self.stopped())
        }
    }

    /// Stops the pipeline now, as a flush or a teardown does.
    ///
    /// # Errors
    ///
    /// Returns [`LaunchError::Stopped`] when the program cannot be ended.
    pub fn stop(&self) -> Result<(), LaunchError> {
        lock(&self.input).take();
        let mut child = lock(&self.child);
        child.kill().map_err(|error| LaunchError::Stopped {
            reason: error.to_string(),
        })?;
        let _ = child.wait();
        Ok(())
    }

    /// Whether the pipeline is still running.
    pub fn is_running(&self) -> bool {
        matches!(lock(&self.child).try_wait(), Ok(None))
    }

    /// Waits, briefly, for a pipeline that was refused to say so.
    ///
    /// The wait is bounded by `gst-launch`'s own startup: a description it refuses is
    /// reported before it would have started playing anything, and one it accepts never
    /// exits at all.
    fn settle(&self) {
        // How long a pipeline gets to say it is refused. Measured rather than guessed:
        // `gst-launch` rejects a bad pipeline (`nope ! fakesink`, and one naming a sink
        // that does not exist) after 81 to 91 ms, because what takes the time is the
        // process starting rather than the pipeline being checked; a good one reaches
        // PLAYING in 82 to 85 ms. This waits a little past the slowest of those.
        //
        // It was 400 ms, and that cost every session a quarter of a second before its
        // first frame for nothing: a client is already sending by the time a mirror
        // stream opens, so this is time the picture spends not being decoded.
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(150);
        let deadline = std::time::Instant::now() + SETTLE;
        while std::time::Instant::now() < deadline {
            if !self.is_running() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// What the program said, for a pipeline that has stopped.
    fn stopped(&self) -> LaunchError {
        lock(&self.input).take();
        let _ = lock(&self.child).wait();
        let reason = std::fs::read_to_string(&self.stderr)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let reason = if reason.is_empty() {
            format!("{} exited without saying why", self.program)
        } else {
            reason
        };
        // A pipeline that was refused never got as far as running, and a caller starting a
        // stream wants to know that rather than "it stopped".
        if reason.contains("erroneous pipeline") || reason.contains("no element") {
            LaunchError::Rejected { reason }
        } else {
            LaunchError::Stopped { reason }
        }
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // A pipeline left running would hold a sound card or a window for as long as the
        // process lives, and a session that forgot to close one is not a reason to keep
        // playing.
        lock(&self.input).take();
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // The file it wrote to goes with it: what a pipeline said is for the failure that
        // was just reported, not for a temporary directory to accumulate. A caller that
        // asked to keep it has said it wants to read it.
        if std::env::var_os(KEEP_STDERR_ENV).is_some() {
            eprintln!(
                "rust-airplay: {} left its output at {}",
                self.program,
                self.stderr.display()
            );
        } else {
            let _ = std::fs::remove_file(&self.stderr);
        }
    }
}

/// A file for a pipeline's output, and its path.
///
/// GStreamer writes a good deal when something goes wrong, which is exactly when the
/// reason matters, so its output goes somewhere it can all be read afterwards. A pipe
/// nobody is reading fills and blocks the program writing to it, and a windowed
/// application has no console to inherit, so a file is the one place that always works.
/// Whether to keep a pipeline's output and say where it is.
///
/// `gst-launch-1.0 -q` is quiet about everything except what is wrong, and what is wrong
/// with a pipeline that takes frames and makes no sound is exactly what its output says.
/// Without this the file goes with the pipeline, which is right for a session and useless
/// for a diagnosis.
pub const KEEP_STDERR_ENV: &str = "AIRPLAY_GST_KEEP_STDERR";

fn open_stderr() -> Result<(std::path::PathBuf, Stdio), LaunchError> {
    // One file per pipeline and not one per process: a session runs two, and a name they
    // would share is a name that loses whichever wrote to it first.
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let which = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("airplay-gst-{}-{which}.log", std::process::id()));
    let file = std::fs::File::create(&path).map_err(|error| LaunchError::StartFailed {
        reason: format!("cannot write {}: {error}", path.display()),
    })?;
    Ok((path, Stdio::from(file)))
}

/// Locks a lock this pipeline owns, ignoring poisoning.
///
/// A pipeline whose writer panicked once is still a pipeline, and the alternative — a
/// stream that can never be written to again — is worse than one written to by whoever is
/// left.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
