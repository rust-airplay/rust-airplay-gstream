// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding the program a pipeline runs in.

use std::process::{Command, Stdio};

use super::LaunchError;

/// The program a pipeline runs in.
pub(crate) const LAUNCH_PROGRAM: &str = "gst-launch-1.0";

/// Names to try for [`LAUNCH_PROGRAM`], in order, for a machine that has it under another
/// one.
const LAUNCH_NAMES: &[&str] = &["gst-launch-1.0", "gst-launch-1.0.exe"];

/// Environment variable naming the program to run, for a runtime this backend would not
/// find on its own.
pub(crate) const LAUNCH_ENV: &str = "AIRPLAY_GST_LAUNCH";

/// Whether to keep a pipeline's output and say where it is.
///
/// The program is quiet about everything except what is wrong, and what is wrong
/// with a pipeline that takes frames and shows nothing is exactly what its output
/// says. Without this the file goes with the pipeline, which is right for a session
/// and useless for a diagnosis.
pub(crate) const KEEP_STDERR_ENV: &str = "AIRPLAY_GST_KEEP_STDERR";

/// The program to run, or the one [`LAUNCH_ENV`] names.
///
/// # Errors
///
/// Returns [`LaunchError::NotFound`] when no name can be run. Finding one is a search of
/// the PATH by the operating system when the process starts, so this asks each candidate
/// for its version once: a name that answers is a name that works, and one that does not
/// is passed over rather than reported as the reason a session has no video.
pub(crate) fn find_launcher() -> Result<String, LaunchError> {
    let named = std::env::var(LAUNCH_ENV)
        .ok()
        .filter(|name| !name.is_empty());
    let names: Vec<String> = match &named {
        Some(name) => vec![name.clone()],
        None => LAUNCH_NAMES.iter().map(|name| (*name).to_owned()).collect(),
    };
    let mut tried = Vec::with_capacity(names.len());
    for name in &names {
        let started = Command::new(name)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match started {
            Ok(status) if status.success() => {
                log::debug!("running pipelines in {name}");
                return Ok(name.clone());
            }
            Ok(status) => tried.push(format!("{name} (exited {status})")),
            Err(error) => tried.push(format!("{name} ({error})")),
        }
    }
    Err(LaunchError::NotFound { tried })
}
