// SPDX-License-Identifier: GPL-3.0-or-later
//! Why a pipeline could not be run, and what the render boundary makes of it.

use super::launch::LAUNCH_PROGRAM;

/// Why a pipeline could not be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchError {
    /// No gst-launch was found, with the names tried.
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
    /// The pipeline names an element this machine does not have.
    ///
    /// Kept apart from [`LaunchError::Rejected`] because the caller's answer differs:
    /// a missing decoder is the environment (unavailable), a broken description is the
    /// request (failed).
    MissingElement {
        /// What the program said about it.
        reason: String,
    },
    /// The pipeline stopped while it was running.
    Stopped {
        /// What the program said about it.
        reason: String,
    },
}

impl LaunchError {
    /// Whether the reason is the machine rather than the request.
    pub(crate) fn is_unavailable(&self) -> bool {
        matches!(self, Self::NotFound { .. } | Self::MissingElement { .. })
    }
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
            Self::MissingElement { reason } => {
                write!(
                    formatter,
                    "the pipeline needs an element this machine lacks: {reason}"
                )
            }
            Self::Stopped { reason } => write!(formatter, "the pipeline stopped: {reason}"),
        }
    }
}

/// The render boundary's view of why a pipeline could not run.
///
/// The split is the one the core's documentation draws: a machine that lacks gst-launch
/// or an element has not met the request's fault, so it is unavailable; a description the
/// program refused or a pipeline that stopped is a request that could not be carried out,
/// so it failed. A session whose machine cannot decode keeps serving its other media
/// rather than treating the client as broken.
pub(crate) fn as_render_error(error: LaunchError) -> rust_airplay::RenderError {
    let text = error.to_string();
    if error.is_unavailable() {
        rust_airplay::RenderError::Unavailable(text)
    } else {
        rust_airplay::RenderError::Failed(text)
    }
}
