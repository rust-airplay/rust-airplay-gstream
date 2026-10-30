// SPDX-License-Identifier: GPL-3.0-or-later
//! Where a pipeline's output goes.

use std::path::{Path, PathBuf};

use super::LaunchError;
use super::launch::KEEP_STDERR_ENV;

/// How much of a pipeline's output is read back, from the start.
///
/// A pipeline that fails says why in a few lines; the rest is element chatter, and
/// reading it all into memory is a way for a decoder to make the receiver allocate.
const MOST: u64 = 64 * 1024;

/// The file one pipeline's output goes to, and whether to keep it.
///
/// GStreamer writes a good deal when something goes wrong, which is exactly when the
/// reason matters, so its output goes somewhere it can all be read afterwards. A pipe
/// nobody is reading fills and blocks the program writing to it, and a windowed
/// application has no console to inherit, so a file is the one place that always works.
#[derive(Debug)]
pub(super) struct StderrFile {
    path: PathBuf,
    keep: bool,
}

impl StderrFile {
    /// Claims a file no other pipeline is writing to.
    ///
    /// The name is unique by construction rather than by luck: creation is
    /// exclusive, so two receivers starting at the same moment cannot end up
    /// sharing one log, and a name already taken is passed over.
    pub(super) fn create() -> Result<Self, LaunchError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let keep = std::env::var_os(KEEP_STDERR_ENV).is_some();
        for _ in 0..8 {
            let which = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("airplay-gst-{}-{which}.log", std::process::id()));
            let opened = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path);
            match opened {
                Ok(file) => {
                    restrict(&file, &path);
                    return Ok(Self { path, keep });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(LaunchError::StartFailed {
                        reason: format!("cannot write {}: {error}", path.display()),
                    });
                }
            }
        }
        Err(LaunchError::StartFailed {
            reason: "cannot find a free name for a pipeline log".to_owned(),
        })
    }

    /// Opens the file for a child process to write to.
    pub(super) fn open(&self) -> Result<std::fs::File, LaunchError> {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|error| LaunchError::StartFailed {
                reason: format!("cannot write {}: {error}", self.path.display()),
            })
    }

    /// What the pipeline said, up to [`MOST`] bytes of it.
    ///
    /// A pipeline whose output cannot be read said nothing: the empty string is a
    /// better answer than a failure to read a diagnostic.
    pub(super) fn read(&self) -> String {
        use std::io::Read;
        let Ok(file) = std::fs::File::open(&self.path) else {
            return String::new();
        };
        let mut text = String::new();
        let _ = file.take(MOST).read_to_string(&mut text);
        text.trim().to_owned()
    }
}

impl Drop for StderrFile {
    fn drop(&mut self) {
        if self.keep {
            log::debug!("a pipeline's output was kept at {}", self.path.display());
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Makes the log readable by this user alone.
///
/// What a decoder says can quote the stream's own parameters, and a session's
/// media is not a public fact: the file is created 0600 on a platform that has
/// the concept, before the child is given it.
#[cfg(unix)]
fn restrict(file: &std::fs::File, path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = file.set_permissions(std::fs::Permissions::from_mode(0o600)) {
        log::warn!("cannot restrict {}: {error}", path.display());
    }
}

#[cfg(not(unix))]
fn restrict(_: &std::fs::File, _: &Path) {}
