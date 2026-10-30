# Architecture

Chinese: [ARCHITECTURE.zh-CN.md](ARCHITECTURE.zh-CN.md).

This crate is one implementation of the renderer boundary that
[`rust-airplay`](https://github.com/rust-airplay/rust-airplay) defines. It decodes coded
frames and plays them; it does not parse AirPlay, open sockets, or know what a `SETUP` is.

## Two media, two shapes

The two streams arrive in different forms, and the backend plays them in different ways:

| | Audio | Video |
| --- | --- | --- |
| Frame boundary | one push is one frame; a coded frame has no start code | start codes in the byte stream |
| How it plays | linked `libgstreamer`, one buffer per frame through `appsrc` | `gst-launch-1.0` as a subprocess, frames written to its standard input |
| Why | a raw AAC-ELD frame has neither a sync word nor a sample rate, so a byte stream cannot say where one frame ends | H.264 and H.265 carry start codes, so a decoder finds its own boundaries, and a decoder in another process cannot take the receiver down with it |

Both choices are in the crate's module documentation as well. The audio path is the one that
links a C library: `gstreamer` and `gstreamer-app` are used in-process, which is why
building this crate needs the GStreamer development files while using it needs only the
runtime plugins.

```text
audio:  appsrc name=audio_source ! queue ! avdec_aac ! audioconvert ! audioresample ! volume ! <sink>
video:  fdsrc ! h264parse ! avdec_h264 ! <sink> sync=false
```

## What the modules are

| Module | What it answers |
| --- | --- |
| `lib.rs` | the crate's public surface: `Options`, `GstRenderer`, the sink constants, and a hidden `testing` module the integration tests build pipelines with |
| `options` | what a caller may choose, and how the environment is read into it |
| `options::sink` | what a sink fragment may name |
| `renderer` | the `Renderer` implementation: opening streams, the volume, the warm pipeline |
| `video` | the video stream: the arguments of its pipeline, its pacing, its flush |
| `audio::engine` | one audio stream: `appsrc`, timestamps, flush, close, Drop |
| `audio::thread` | the GLib main loop one pipeline runs on |
| `audio::graph` | the caps and description an audio pipeline is built from |
| `audio::clock` | when a frame is due, from the client's sample counter |
| `pipeline` | one `gst-launch-1.0` subprocess: start, push, finish, restart |
| `pipeline::writer` | the thread and queue that write to its standard input |
| `pipeline::stderr` | where its output goes |
| `pipeline::error` | why it could not run, and what the render boundary makes of it |

The split is by question rather than by size: a file here is the code that answers one
question, and the `#[cfg(test)]` tests sit next to what they test.

## The video pipeline

The arguments are built as a list, not as one string and not by splitting one: a caps filter
contains commas and spaces, and every platform quotes those differently, so a token list is
the only form that survives `Command` on both Windows and Unix. `gst-launch` joins its
arguments again, so the pipeline is the same one.

The sink is a fragment -- an element and its properties -- and it is checked when the renderer
is built rather than when the first frame arrives. A fragment may not contain a chain
separator or quoting, and its first token must name an installed element: pasting a caller's
string into a pipeline description is how a configuration option becomes a second branch of
the pipeline, and a `filesink` in that branch is a way to write a session to disk.

Writing to a subprocess can block for as long as the subprocess declines to read, so the
writing happens on a thread of its own behind a queue a few buffers deep. A push hands over
one buffer and waits a bounded time for the writer to take it; a pipeline that has stopped
reading becomes a refused frame rather than a session that never returns. Every wait for the
process has a deadline as well: a closed pipeline gets time to play out, a killed one gets
time to be reaped, and past either it is killed rather than waited for.

## What a frame does

- **Video** is written to the pipeline in the order it was pushed. Timestamps are not carried
  into the pipeline: the subprocess is fed a byte stream, and a byte stream has no timestamps.
  Pacing therefore happens on this side. `VideoStream::set_pace` sleeps between frames, and
  the core only calls it when `Config::pace_video` is off, because otherwise the core does
  the waiting.
- **Audio** keeps its timestamps: each frame becomes an `appsrc` buffer whose presentation
  time is the sample offset from the session's first frame, at the session's sample rate. The
  offset is kept in the client's own 32 bits and subtracted with wrapping, because an audio
  timestamp is an RTP counter that wraps: a receiver that widened it would stop measuring
  elapsed time after a long session. The pipeline uses `sync=false` because a client's
  clock and a sound card's clock are not the same clock; waiting on one to schedule the other
  is what makes a receiver stutter.
- **Volume** is a `volume` element directly in front of the sink. `Renderer::set_volume`
  stores the gain for streams opened later and sets the property of the element that is
  playing now. The core speaks linear gain, never decibels.

## Flushing

A flush means what a seek means: what has not been played is dropped, and the decoder forgets
the frames it would otherwise refer to.

- **Audio** goes back to `READY` and then to `PLAYING`. An element's state can be
  changed from any thread, so this needs nothing from the pipeline's own loop, and `READY`
  is what clears every queue between the source and the sink. The first-frame anchor is
  dropped with it, so the next frame starts a new timeline.
- **Video** replaces the decoder process. A `gst-launch` pipeline has no flush signal, and
  a decoder cannot be told to forget what it has queued, so the process that holds it is
  stopped and started again with the same arguments. This costs a pipeline start, which is
  the honest price of dropping queued media. The key frame a client sends after a flush is
  what makes the new decoder correct rather than merely empty.

## Warming

Starting a video pipeline costs about 700 ms before it can accept a frame: the window and the
graphics device come up when the pipeline starts, not when the first frame arrives.
`warm_video` starts it with the receiver so the cost is paid before a client connects, and a
warm pipeline is reused by the next session instead of being torn down. The renderer keeps its
own handle on that pipeline as well as the stream's, so that closing a session leaves the
pipeline running; only a coding it cannot play, or a process that has died, makes it start a
new one. It is off by default because a warm pipeline is a window on screen before anything is
mirrored, which a television does but a desktop application may not.

## Closing

`close` ends the audio stream with an end-of-stream event and waits for the pipeline to
reach the end of the stream, so the sound card's own queue is part of the session; past a
couple of seconds it stops waiting. `Drop` ends the stream if `close` was never called,
tells the pipeline thread to stop, and waits for it: a stream whose thread outlived it would
hold a sound card for as long as the process lives. The video side is the same shape -- `close`
finishes the process unless the pipeline is warm, and `Drop` stops it.

## Failure

`RenderError::Unavailable` is used when the machine cannot do what was asked: no
`gst-launch-1.0` on `PATH`, a sink or decoder element that is not installed, no sound card.
`RenderError::Failed` is used when the pipeline accepted the request and then failed. A
missing element is the first kind even when it is discovered from `gst-launch`'s own output,
because the caller's answer differs: a session whose machine cannot decode should learn that
the environment is short, not that its client is broken. The receiver reports either and
keeps serving; a backend that retried inside itself would be hiding its own failures. A
machine without `gst-launch-1.0` can still play audio: the renderer builds, and only a video
request reports that it is unavailable.

The subprocess's standard error goes to a temporary file rather than a pipe: a pipe nobody
reads fills and blocks the child, and the output is exactly what is wanted when a pipeline
fails. The file is created exclusively, so two receivers starting at once cannot share one
log, it is created 0600 where the platform has permissions, because a decoder can quote the
stream's own parameters, and at most 64 KiB of it is read back. `AIRPLAY_GST_KEEP_STDERR`
keeps the file and logs its path, which is how a failure is diagnosed without a console.

## Tests

The unit tests build pipeline descriptions and check the decisions that are easy to get wrong:
that HEVC gets an HEVC chain, that every pipeline names a parser and a decoder, that a sink
fragment cannot smuggle a branch in, that a timestamp wraps where the client's counter wraps.
They need no GStreamer runtime.

The integration tests push real bytes through a real pipeline: an Annex-B stream into a
`filesink`, forty raw AAC-ELD payloads into an audio pipeline, and a session's worth of
frames through the same path the renderer builds. They are `#[ignore]`d because they need a
GStreamer runtime, and they are the ones that catch a pipeline that accepts every buffer and
decodes none. The video fixture is synthetic and [tools/make-fixtures.sh](../tools/make-fixtures.sh)
regenerates it byte for byte; the audio one is captured, because no AAC-ELD encoder exists to
synthesize it. [`tests/fixtures/README.md`](../tests/fixtures/README.md) records what each file
is.

```sh
cargo test                  # unit tests
cargo test -- --ignored     # needs a GStreamer runtime
```
