# Architecture

Chinese: [ARCHITECTURE.zh-CN.md](ARCHITECTURE.zh-CN.md).

This crate is one implementation of the renderer boundary that
[`rust-airplay`](https://github.com/rust-airplay/rust-airplay) defines. It decodes coded frames
and plays them; it does not parse AirPlay, open sockets, or know what a `SETUP` is.

## Two media, two shapes

The two streams arrive in different forms, and the backend plays them in different ways:

| | Audio | Video |
| --- | --- | --- |
| Frame boundary | one push is one frame; a coded frame has no start code | start codes in the byte stream |
| How it plays | linked `libgstreamer`, one buffer per frame through `appsrc` | `gst-launch-1.0` as a subprocess, frames written to its stdin |
| Why | a raw AAC-ELD frame has neither a sync word nor a sample rate, so a byte stream cannot say where one frame ends | H.264 and H.265 carry start codes, so a decoder finds its own boundaries; a subprocess keeps the backend free of C compilation and of unsafe code |

Both choices are in the crate's module documentation as well. The audio path is the one that
links a C library: `gstreamer` and `gstreamer-app` are used in-process, which is why building
this crate needs the GStreamer development files while using it needs only the runtime
plugins.

```text
audio:  appsrc ! queue ! avdec_aac ! audioconvert ! audioresample ! volume ! <sink>
video:  fdsrc ! h264parse ! avdec_h264 ! videoconvert ! video/x-raw,format=I420 ! <sink>
```

The exact video chain depends on the sink, because a sink and a decoder are one choice:
`d3d11h264dec` decodes into Direct3D11 memory that only `d3d11videosink` can take, and every
other sink needs `videoconvert` in front of it. `Options::with_video_sink` replaces the sink and
the chain together, which is why a caller should use it rather than setting the two fields
separately.

## What a frame does

- **Video** is written to the pipeline in the order it was pushed. Timestamps are not carried
  into the pipeline: the subprocess is fed a byte stream, and a byte stream has no timestamps.
  Pacing therefore happens on this side. `VideoStream::set_pace` sleeps between frames when the
  core asks for a spacing, and the core normally asks for none (`Config::pace_video` is off).
- **Audio** keeps its timestamps: each frame becomes an `appsrc` buffer whose presentation time
  is derived from the client's sample counter, so the sink plays it at the rate it was
  captured. The pipeline uses `sync=false` because a client's clock and a sound card's clock
  are not the same clock; waiting on one to schedule the other is what makes a receiver
  stutter.
- **Volume** is a `volume` element directly in front of the sink. `Renderer::set_volume` stores
  the gain for streams opened later and sets the property of the element that is playing now.
  The core speaks linear gain, never decibels.

## Warming

Starting a video pipeline costs about 700 ms before it can accept a frame: the window and the
graphics device come up when the pipeline starts, not when the first frame arrives. `warm_video`
starts it with the receiver so the cost is paid before a client connects, and a warm pipeline is
reused by the next session instead of being torn down. It is off by default because a warm
pipeline is a window on screen before anything is mirrored, which a television does but a
desktop application may not.

## Flushing and closing

`flush` is a no-op on both streams, and that is a real limitation rather than a decision:

- a `gst-launch-1.0` pipeline is a process fed by a pipe; there is no in-band flush signal for
  a byte stream, so frames already written are decoded and shown;
- an `appsrc` pipeline cannot discard buffers already pushed without ending the stream.

What limits the damage is the decoder itself: after a gap, a video decoder resynchronises at
the next key frame, and the core's queue restarts there for the same reason. The practical
effect is that a flush can leave up to the pipeline's buffered frames on screen for a moment.
A backend that needs an exact flush must own its pipeline in-process, which is what a future
in-process video path would do.

`close` ends the audio stream with an end-of-stream event so the decoder drains, and closes the
video pipeline only when it is not warm. `Drop` stops both: a pipeline left running would hold
a sound card or a window for as long as the process lives.

## Failure

`RenderError::Unavailable` is used when the machine cannot do what was asked: no
`gst-launch-1.0` on `PATH`, a sink or decoder element that is not installed, no sound card.
`RenderError::Failed` is used when the pipeline accepted the request and then failed. The
receiver reports either and keeps serving; a backend that retried inside itself would be hiding
its own failures.

The subprocess's standard error goes to a temporary file rather than a pipe: a pipe nobody reads
fills and blocks the child, and the output is exactly what is wanted when a pipeline fails.
`AIRPLAY_GST_KEEP_STDERR=1` keeps that file and prints its path when the pipeline ends, which is
how a failure is diagnosed without a console.

## Tests

The unit tests build pipeline descriptions and check the decisions that are easy to get wrong:
that a sink brings its decoder with it, that HEVC gets an HEVC chain, that every pipeline names a
parser and a decoder. They need no GStreamer.

The integration tests capture frames from a real session — an H.264 key frame and the following
frames, an AAC-ELD payload, a measured one-second stream — and push them through a real pipeline
into `fakesink`. They are `#[ignore]`d because they need a GStreamer runtime, and they are the
ones that catch a pipeline that accepts every buffer and decodes none. Run them with:

```sh
cargo test -- --ignored
```
