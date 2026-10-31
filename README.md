# rust-airplay-gstream

[![CI](https://github.com/rust-airplay/rust-airplay-gstream/actions/workflows/ci.yml/badge.svg)](https://github.com/rust-airplay/rust-airplay-gstream/actions/workflows/ci.yml)

The GStreamer backend for [`rust-airplay`](https://github.com/rust-airplay/rust-airplay).

`rust-airplay` receives an AirPlay session, decrypts it and hands over coded frames. This crate
decodes them and plays them: audio through a linked GStreamer pipeline, video through a
`gst-launch-1.0` subprocess. It implements the core's `Renderer` trait, so a program depends on
the core for the protocol and on this crate for playback.

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) explains why the two media take different
shapes, what happens to a frame, and what a flush can and cannot do here.

[CHANGELOG.md](CHANGELOG.md) records what 0.3 changed, and [MIGRATION.md](MIGRATION.md) maps a
0.2 build onto it.

## Requirements

**Building** needs the GStreamer development files, found with `pkg-config`: `gstreamer-1.0`
and `gstreamer-app-1.0`.

**Running** needs the GStreamer runtime and its plugins:

| Purpose | Elements or program |
| --- | --- |
| Video | `gst-launch-1.0`, `h264parse` or `h265parse`, `avdec_h264` or `avdec_h265`, a sink |
| Audio | `avdec_aac` or `avdec_alac`, `audioconvert`, `audioresample`, a sink |

The `avdec_*` decoders come from `gst-libav`. To run only audio, a machine needs no
`gst-launch-1.0`: the renderer builds, audio plays, and a video request reports that it is
unavailable.

On Windows, MSYS2 UCRT64 provides both; put `C:\msys64\ucrt64\bin` on `PATH`. On Debian and
Ubuntu, `libgstreamer1.0-dev` and `libgstreamer-plugins-base1.0-dev` build it, and the plugin
packages for the elements above run it.

## Using it

```toml
[dependencies]
rust-airplay = "0.3"
rust-airplay-gstream = "0.3"
```

```rust,no_run
use std::sync::Arc;

use rust_airplay::{Config, Renderer};

# async fn start() -> Result<(), Box<dyn std::error::Error>> {
let renderer: Arc<dyn Renderer> =
    Arc::new(rust_airplay_gstream::GstRenderer::new().expect("a GStreamer runtime"));
let config = Config::new("Living room", [0x42; 32], renderer);
rust_airplay::run(config).await?;
# Ok(())
# }
```

Both crates are on crates.io. `rust-airplay-gstream` follows the core's 0.3 line, so a
`Cargo.lock` resolves the pair together. Its floor is `rust-airplay` 0.3.1: 0.3.0's receiver
delegated pacing to the backend and trimmed its mirror queue one frame at a time, and 0.3.1
is the release that fixed both.

## Options

Everything a caller can choose is part of `Options`, and every default is what the
corresponding environment variable would produce, so `Options::default` behaves exactly like
`GstRenderer::new`:

| Method | Meaning | Default |
| --- | --- | --- |
| `Options::new(audio, video)` | the two sinks, spelled out | -- |
| `.with_audio_sink(sink)` | GStreamer audio sink | first installed of `wasapisink`, `autoaudiosink`, `osssink`, `alsasink`, `pulsesink`, `fakesink` |
| `.with_video_sink(sink)` | GStreamer video sink | first installed of `d3d11videosink`, `autovideosink`, `glimagesink`, `fakesink` |
| `.with_video_chain(codec, parser, decoder)` | the parser and decoder for H.264 or H.265 | `h264parse` + `avdec_h264`, `h265parse` + `avdec_h265` |
| `.with_warm_video(bool)` | keep the video pipeline between sessions | off |
| `.with_pacing(bool)` / `.without_pacing()` | pace video frames on a cumulative timeline | on |

A sink is an element and, optionally, its properties -- `fakesink`, or
`filesink location=out.raw`. It is checked when the renderer is built: the element must exist,
and the fragment must be a single element rather than a pipeline of its own, so a sink cannot
pull `!`-separated elements or another sink into the pipeline this backend builds.

```rust
use rust_airplay::{Renderer, VideoCodec};
use rust_airplay_gstream::{GstRenderer, Options};

let renderer = GstRenderer::with_options(
    Options::new("fakesink", "filesink location=out.raw")
        .with_video_chain(VideoCodec::H265, "h265parse", "avdec_h265")
        .with_warm_video(true),
)?;
# Ok::<(), rust_airplay::RenderError>(())
```

## Environment

| Variable | Effect |
| --- | --- |
| `AIRPLAY_GST_LAUNCH` | the `gst-launch-1.0` executable to run |
| `AIRPLAY_GST_AUDIO_SINK` | audio sink, as a fragment |
| `AIRPLAY_GST_VIDEO_SINK` | video sink, as a fragment |
| `AIRPLAY_GST_VIDEO_WARM` | `1` (or any value but `0`/`false`/`no`/`off`) keeps the video pipeline between sessions |
| `AIRPLAY_GST_KEEP_STDERR` | keep the subprocess's output instead of deleting it, and log its path |

`Options` reads these when it is constructed, so a program that wants to choose in code
uses `Options::new` and the `with_*` methods instead.

## What it does with the stream

- **Video** is written to the subprocess in the order it was pushed. A byte stream carries no
  timestamps, and with `rust-airplay` 0.3.1 and later the receiver does the pacing itself (or
  pushes at full speed and lets the pipe push back); it does not call `VideoStream::set_pace`.
  That hook remains for a caller driving the trait directly, and this backend honours the
  contract the trait documents: the wait is scheduled against a cumulative deadline, so a sleep
  that returns late does not push every later frame back with it; a spacing of zero means "do
  not wait" rather than "sleep for zero"; a stream that falls more than one interval behind is
  re-anchored instead of repaying the gap as a burst; and `Options::without_pacing()` turns the
  wait off for a sink that schedules on its own clock.
- **Audio** keeps its timing: each frame is an `appsrc` buffer whose presentation time is its
  sample offset from the first frame of the session, at the session's sample rate.
- **Volume** is a linear gain on a `volume` element in front of the sink, applied to the stream
  that is playing and remembered for the next one.
- **flush** drops what is queued: audio goes back to `READY` and starts again, and video replaces
  the decoder process, which is the only way to make a decoder forget its queue. The key frame a
  client sends after a flush is what makes the new decoder correct rather than merely empty.

## Failure

`RenderError::Unavailable` means the machine cannot do it: no `gst-launch-1.0`, a sink or
decoder element that is not installed, no GStreamer runtime. `RenderError::Failed` means the
request could not be carried out: a description the program refused, a pipeline that stopped.
The receiver reports either and keeps serving; a backend that retried inside itself would be
hiding its own failures.

## Tests

```sh
cargo test                          # unit tests, no GStreamer needed
cargo test -- --ignored             # needs a GStreamer runtime and gst-launch-1.0
```

The integration tests decode a fixture and count the frames: a pipeline can accept every buffer
and still decode none, and only decoded output settles that. The video fixture is synthetic --
`videotestsrc` through `openh264enc`, 640x360, 48 pictures -- so it carries no captured screen,
and [tools/make-fixtures.sh](tools/make-fixtures.sh) regenerates it byte for byte. The audio
fixture is forty raw AAC-ELD payloads from a real session, which is kept because no AAC-ELD
encoder exists to synthesize them. [`tests/fixtures/README.md`](tests/fixtures/README.md) records
what each file is and how to reproduce it.

## Releasing

Publishing is a workflow that runs from a tag, `vX.Y.Z`, whose version matches `Cargo.toml`. It
refuses a dirty tree, runs the full test suite including the ignored tests, checks that the
packaged `.crate` contains the fixtures, and dry-runs the publish before publishing with the
`CARGO_REGISTRY_TOKEN` repository secret. Publish the core release this crate depends on first:
Cargo resolves `rust-airplay = "0.3"` from crates.io while packaging this one.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).

Chinese: [README.zh-CN.md](README.zh-CN.md).
