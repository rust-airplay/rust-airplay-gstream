// SPDX-License-Identifier: GPL-3.0-or-later
//! The audio stream: an appsrc the session pushes frames into.
//!
//! Audio cannot be delivered the way video is. A socket is a stream of bytes either way,
//! but H.264 has start codes, so a decoder can find its frames in a run of them, and raw
//! AAC-ELD has no such thing: a frame carries no sample rate of its own and there is no
//! sync word to split on. The only way a decoder can be given these frames is one buffer
//! per frame.
//!
//! appsrc is that buffer, and it is why audio is linked into this process while video is
//! still run as a gst-launch subprocess.
//!
//! The parts are kept apart because each answers a different question: clock is when a
//! frame is due, graph is what plays it, thread is the loop that owns the pipeline, and
//! engine is the stream that feeds it and stops it.

mod clock;
mod engine;
mod graph;
mod thread;

pub(crate) use engine::GstAudio;
