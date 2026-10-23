# rust-airplay-gstream

The GStreamer backend for [`rust-airplay`](https://github.com/rust-airplay/rust-airplay).

`rust-airplay` receives an AirPlay session, decrypts it and hands over coded frames. This
crate decodes them and plays them: audio through a linked GStreamer pipeline, video through
a `gst-launch-1.0` subprocess. It implements the `Renderer` trait, so a program depends on
the core for the protocol and on this crate for playback.

## Requirements

Building needs the GStreamer development files, found with `pkg-config`: `gstreamer-1.0`
and `gstreamer-app-1.0`. Running needs a GStreamer runtime with the elements a session
names:

| Purpose | Elements |
| --- | --- |
| Video | `h264parse` or `h265parse`, `avdec_h264` or `avdec_h265` (or the Direct3D11 pair), `videoconvert`, a sink |
| Audio | `avdec_aac` or `avdec_alac`, `audioconvert`, `audioresample`, a sink |

On Windows, MSYS2 UCRT64 provides both; put `C:\msys64\ucrt64\bin` on `PATH`. On Debian and
Ubuntu, `libgstreamer1.0-dev` and `libgstreamer-plugins-base1.0-dev` build it, and the
plugin packages for the elements above run it.

## Using it

```toml
[dependencies]
rust-airplay = { git = "https://github.com/rust-airplay/rust-airplay", branch = "main" }
rust-airplay-gstream = { git = "https://github.com/rust-airplay/rust-airplay-gstream", branch = "main" }
```

```rust,no_run
use std::sync::Arc;
use rust_airplay::Renderer;

# fn example() {
let renderer: Arc<dyn Renderer> = Arc::new(
    rust_airplay_gstream::GstRenderer::new().expect("a GStreamer runtime"),
);
# }
```

The choices a caller can make are in `Options`: the two sinks, the video parser and decoder,
and whether to warm the video pipeline. Each default comes from the environment, so
`Options::default()` behaves exactly like `GstRenderer::new`. `Options::with_video_sink`
keeps the decoder with the sink, because `d3d11h264dec` decodes into memory only
`d3d11videosink` accepts.

## Environment

| Variable | Effect |
| --- | --- |
| `AIRPLAY_GST_LAUNCH` | the `gst-launch-1.0` executable to run |
| `AIRPLAY_GST_AUDIO_SINK` | audio sink; `wasapisink` on Windows |
| `AIRPLAY_GST_VIDEO_SINK` | video sink; `d3d11videosink` where it exists, `autovideosink` otherwise |
| `AIRPLAY_GST_VIDEO_WARM` | set to `1` to start the video pipeline with the receiver |
| `AIRPLAY_GST_KEEP_STDERR` | keep the subprocess's error output instead of redirecting it to a file |

## Tests

```sh
cargo test
cargo test -- --ignored
```

The ignored tests need a GStreamer runtime. They decode frames captured from a real session,
which is what makes them useful: a pipeline can accept every buffer and still decode none,
and only the decoded output settles the question.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).

Chinese: [README.zh-CN.md](README.zh-CN.md).
