// SPDX-License-Identifier: GPL-3.0-or-later
//! The thread one audio pipeline runs on.
//!
//! Everything here happens inside the pipeline's own main context: GStreamer gives an
//! element the thread-default context of whoever creates it, and a bus watch added to a
//! context nobody runs never reports anything. The context is entered with
//! with_thread_default and the loop runs inside it, which is the whole of the arrangement.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSrc;

/// Where a pipeline's failure is kept, so a push can say why nothing is being played.
pub(super) type Failure = Arc<Mutex<Option<String>>>;

/// What the pipeline thread hands back once the pipeline is playing.
pub(super) struct Opened {
    pub(super) appsrc: AppSrc,
    pub(super) volume: gst::Element,
    pub(super) pipeline: gst::Pipeline,
}

/// Runs one pipeline until it is told to stop, reporting its first failure.
///
/// Everything here happens inside the pipeline's own main context: GStreamer gives an
/// element the thread-default context of whoever creates it, and a bus watch added to a
/// context nobody runs never reports anything. The context is entered with
/// with_thread_default and the loop runs inside it, which is the whole of the
/// arrangement.
pub(super) fn run(
    description: &str,
    caps: gst::Caps,
    failure: Failure,
    ended: Arc<(Mutex<bool>, Condvar)>,
    ready: Sender<Result<Opened, String>>,
    stopping: Receiver<()>,
) {
    let context = gst::glib::MainContext::new();
    let loop_ = gst::glib::MainLoop::new(Some(&context), false);
    let telling = ready.clone();
    let entered = context.with_thread_default(|| {
        let element = match gst::parse::launch(description) {
            Ok(element) => element,
            Err(error) => {
                let _ = ready.send(Err(format!("the audio pipeline cannot be built: {error}")));
                return;
            }
        };
        let pipeline = match element.downcast::<gst::Pipeline>() {
            Ok(pipeline) => pipeline,
            Err(_) => {
                let _ = ready.send(Err("the audio pipeline is not a pipeline".to_owned()));
                return;
            }
        };
        let source = pipeline
            .by_name("audio_source")
            .and_then(|element| element.downcast::<AppSrc>().ok());
        let Some(appsrc) = source else {
            let _ = ready.send(Err("the audio pipeline has no source".to_owned()));
            return;
        };
        let Some(volume) = pipeline.by_name("volume") else {
            let _ = ready.send(Err("the audio pipeline has no volume element".to_owned()));
            return;
        };

        // What the pipeline is handed, said once and for all: the caps go on the source
        // rather than on a filter downstream, because it is the source that has to say
        // what its own buffers are.
        appsrc.set_caps(Some(&caps));
        appsrc.set_format(gst::Format::Time);

        // The guard is held for as long as the loop runs: dropping it removes the watch.
        let Some(bus) = pipeline.bus() else {
            let _ = ready.send(Err("the audio pipeline has no bus".to_owned()));
            return;
        };
        let _watching = {
            let quiet = Arc::clone(&failure);
            let ended = Arc::clone(&ended);
            let loop_for_bus = loop_.clone();
            match bus.add_watch_local(move |_, message| {
                use gst::MessageView;
                match message.view() {
                    MessageView::Error(error) => {
                        let text = format!(
                            "{}: {}",
                            error
                                .src()
                                .map(|source| source.name().to_string())
                                .unwrap_or_else(|| "the audio pipeline".to_owned()),
                            error.error()
                        );
                        let mut kept = quiet
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if kept.is_none() {
                            *kept = Some(text);
                        }
                        loop_for_bus.quit();
                        gst::glib::ControlFlow::Break
                    }
                    MessageView::Eos(_) => {
                        let (state, condvar) = &*ended;
                        let mut done = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        *done = true;
                        condvar.notify_all();
                        loop_for_bus.quit();
                        gst::glib::ControlFlow::Break
                    }
                    _ => gst::glib::ControlFlow::Continue,
                }
            }) {
                Ok(watching) => watching,
                Err(error) => {
                    let _ = ready.send(Err(format!(
                        "the audio pipeline cannot be watched: {error}"
                    )));
                    return;
                }
            }
        };

        // The thread that stops the pipeline is started before the stream is handed out:
        // a stream whose stop never arrives is a pipeline that holds a sound card until
        // the process ends.
        let loop_for_stop = loop_.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("airplay-gst-audio-stop".to_owned())
            .spawn(move || {
                let _ = stopping.recv();
                loop_for_stop.quit();
            })
        {
            let _ = ready.send(Err(format!("the audio stop thread cannot start: {error}")));
            return;
        }

        if let Err(error) = pipeline.set_state(gst::State::Playing) {
            let _ = ready.send(Err(format!("the audio pipeline will not start: {error}")));
            return;
        }
        // Handed out only once the pipeline is playing, so no frame can be pushed into a
        // pipeline that is not ready for it: appsrc would take them and the state change
        // would drop them, which is a moment of silence at the start of every session.
        let _ = ready.send(Ok(Opened {
            appsrc,
            volume,
            pipeline: pipeline.clone(),
        }));

        loop_.run();
        let _ = pipeline.set_state(gst::State::Null);
    });
    if entered.is_err() {
        let _ = telling.send(Err(
            "the audio pipeline's main context is owned by another thread".to_owned(),
        ));
    }
}
