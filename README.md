# rust-airplay-gstream

The GStreamer backend for [`rust-airplay`](https://github.com/rust-airplay/rust-airplay).

`rust-airplay` receives an AirPlay session, decrypts it and hands over coded frames. This crate
decodes them and plays them: audio through a linked GStreamer pipeline, video through a
`gst-launch-1.0` subprocess. It implements the core's `Renderer` trait, so a program depends on
the core for the protocol and on this crate for playback.

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) explains why the two media take different
shapes, what happens to a frame, and what a flush can and cannot do here.

## Requirements

**Building** needs the GStreamer development files, found with `pkg-config`: `gstreamer-1.0`
and `gstreamer-app-1.0`.

**Running** needs the GStreamer runtime, its plugins, and `gst-launch-1.0` on `PATH` for the
video path:

| Purpose | Elements or program |
| --- | --- |
| Video | `gst-launch-1.0`, `h264parse` or `h265parse`, `avdec_h264` or `avdec_h265` (or the Direct3D11 pair), `videoconvert`, a sink |
| Audio | `avdec_aac` or `avdec_alac`, `audioconvert`, `audioresample`, a sink |

On Windows, MSYS2 UCRT64 provides both; put `C:\msys64\ucrt64\bin` on `PATH`. On Debian and
Ubuntu, `libgstreamer1.0-dev` and `libgstreamer-plugins-base1.0-dev` build it, and the plugin
packages for the elements above run it.

## Using it

```toml
[dependencies]
rust-airplay = { git = "https://github.com/rust-airplay/rust-airplay", branch = "main" }
rust-airplay-gstream = { git = "https://github.com/rust-airplay/rust-airplay-gstream", branch = "main" }
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

The crate is not on crates.io yet: it depends on the core through git, and `publish` is false
until the core has a release.

## Options

Everything a caller can choose is a field of `Options`:

| Field | Meaning | Default |
| --- | --- | --- |
| `audio_sink` | GStreamer audio sink | `wasapisink` on Windows, the first available of `auto`, `oss`, `alsa`, `pulse` elsewhere |
| `video_sink` | GStreamer video sink | `d3d11videosink` where it exists, `autovideosink` otherwise |
| `video_parser` | parser the mirror pipeline starts with | `h264parse`, or `h265parse` for HEVC |
| `video_decoder` | decoder and any converter it needs | chosen with the sink |
| `warm_video` | start the video pipeline with the receiver | off |

Each default is what the corresponding environment variable would produce, so `Options::default`
behaves exactly like `GstRenderer::new`. `Options::with_video_sink` replaces the sink and the
decoder chain together, because `d3d11h264dec` decodes into memory that only
`d3d11videosink` accepts; naming one without the other is how a hardware sink ends up fed by a
software decoder.

## Environment

| Variable | Effect |
| --- | --- |
| `AIRPLAY_GST_LAUNCH` | the `gst-launch-1.0` executable to run |
| `AIRPLAY_GST_AUDIO_SINK` | audio sink; `wasapisink` on Windows |
| `AIRPLAY_GST_VIDEO_SINK` | video sink; `d3d11videosink` where it exists, `autovideosink` otherwise |
| `AIRPLAY_GST_VIDEO_WARM` | set to `1` to start the video pipeline with the receiver |
| `AIRPLAY_GST_KEEP_STDERR` | keep the subprocess's error output instead of deleting it, and print its path |

## What it does with the stream

- **Video** is written to the pipeline in the order it was pushed. The subprocess gets a byte
  stream, so timestamps do not cross into it; pacing happens here, when the core asks for it.
- **Audio** keeps its timestamps: each frame is an `appsrc` buffer whose presentation time comes
  from the client's sample counter, and the sink plays it at the captured rate.
- **Volume** is a linear gain on a `volume` element in front of the sink, applied to the stream
  that is playing and remembered for the next one.

## Tests

```sh
cargo test
cargo test -- --ignored
```

The ignored tests need a GStreamer runtime and a `gst-launch-1.0`. They decode frames captured
from a real session, which is what makes them useful: a pipeline can accept every buffer and
still decode none, and only the decoded output settles that question.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).

Chinese: [README.zh-CN.md](README.zh-CN.md).
