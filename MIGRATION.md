# Migrating from 0.2 to 0.3

Chinese: [MIGRATION.zh-CN.md](MIGRATION.zh-CN.md).

0.3 tracks `rust-airplay 0.3`. The renderer boundary kept its shape, so the work below is
almost all in this backend: a smaller public surface, a checked sink, and errors that say
which side is short. [CHANGELOG.md](CHANGELOG.md) has the reasoning; this page is the
mechanical mapping.

## Dependencies

```toml
rust-airplay = "0.3"
rust-airplay-gstream = "0.3"
```

Whatever pulls the renderer in has to move to 0.3 as well -- a 0.2 core with a 0.3 backend is
two different `Renderer` traits, and the error is a type mismatch at `Config::new`.

## The streams

`AudioStream` is unchanged. `VideoStream` gained `pause` and `resume`, which have default
implementations, so an implementor that does not care compiles as before. This backend leaves
them as the defaults: the sink holds the last frame on its own, so there is nothing for the
backend to freeze.

`AudioFrame::timestamp` is a count of samples, not nanoseconds, and `VideoFrame::timestamp`
is not used by this backend at all -- the subprocess is fed a byte stream, and a byte stream has
no timestamps. If you wrote code that converted a timestamp before pushing, delete the
conversion, or the stream will play at the wrong rate.

`Volume` has both `level` and `fraction`, so `volume.fraction()` still compiles;
`level` is what the 0.3 documentation names, and the two read the same field.

## `Options`

The old type was a set of single-element setters, and one of them set a chain for every
coding. It is now a value with one chain per coding.

| 0.2 | 0.3 |
| --- | --- |
| `Options::default()` | `Options::default()` -- still reads the environment |
| `with_audio_sink(s)` | `with_audio_sink(s)` |
| `with_video_sink(s)` | `with_video_sink(s)` -- no longer touches the decoder chain |
| `with_video_parser(p)` | `with_video_parser(codec, p)` |
| `with_video_decoder(d)` | `with_video_decoder(codec, d)` |
| -- | `with_video_chain(codec, parser, decoder)` |
| `with_warm_video()` | `with_warm_video(bool)` |

There is also `Options::new(audio_sink, video_sink)` for a caller that names both and wants
no environment lookup -- and in 0.3 that is literal: warming is off there and only
`with_warm_video` turns it on, where 0.2's equivalent still read `AIRPLAY_GST_VIDEO_WARM`.
`Options::chain(codec)` returns the `Chain` a coding will use.

```rust
use rust_airplay::{VideoCodec, Volume};
use rust_airplay_gstream::{GstRenderer, Options};

let options = Options::new("autoaudiosink", "autovideosink")
    .with_video_chain(VideoCodec::H265, "h265parse", "avdec_h265")
    .with_warm_video(true);
let renderer = GstRenderer::with_options(options)?;
```

## Sinks

A sink is an element and its properties -- `fakesink`, or `filesink location=out.raw` -- and
it is checked when the renderer is built. A fragment may not contain `!`, and a token after
the element must be `name=value`. A fragment that fails this is an error from
`GstRenderer::new`, `with_sinks` or `with_options`, not from the session that would have
used it:

```rust
// 0.2: built here, failed when a session opened the stream.
// 0.3:
assert!(GstRenderer::with_sinks("fakesink", "no-such-sink").is_err());
```

The reason is that a sink fragment is pasted into a pipeline description, and a caller that
controls it could otherwise add a branch -- a `filesink` writing a mirrored session to disk --
rather than choosing an output.

## The renderer

- `program()` returns `Option<&str>`. `gst-launch-1.0` is needed for video only, so a machine
  without it builds a renderer that can play audio; a video request on it reports
  `RenderError::Unavailable`. Use `find_launcher()` (which returns `Result<String, RenderError>`)
  when a caller needs to know before asking for video.
- `video_is_warm()`, `video_description(codec)`, `volume()`, `audio_sink()` and
  `video_sink()` are as before.
- `with_warm_video` still warms at construction, and a warm request on a machine without
  `gst-launch-1.0` is an `Unavailable` error from the constructor rather than a promise the
  renderer cannot keep.
- A warm pipeline is handed to one stream at a time. A second session that arrives while the
  first still holds it ends the old pipeline and starts its own, rather than sharing a pipe
  whose two writers would interleave two H.264 streams.

## Errors

`RenderError::Unavailable` means the machine cannot do it: no launcher, a sink or decoder not
installed, no sound card, or an audio configuration this backend has no description for.
`Failed` means the pipeline accepted the request and then failed. 0.2 reported a missing
video element as `Failed`; a session that hits that now gets `Unavailable`, and an audio
pipeline that stops after it started reports `Failed` rather than `Unavailable`, so code that
branched on the two may need the other arm.

Audio configurations other than 44.1 kHz stereo are refused here. Both codings carry a fixed
configuration inside `codec_data`, so caps that named another rate or channel count
described a stream no decoder reads correctly; `Config` in core 0.3 uses 44.1 kHz stereo for
every format this crate supports, so a 0.3 session does not hit it.

## Stream lifetime

- `flush` now drops what has not been played. Audio goes through `READY` and back, which
  clears the queues and re-anchors the timeline at the next frame; video stops its decoder
  process and starts another with the same arguments. A flush on video therefore costs a
  pipeline start, and the key frame the client sends afterwards is what makes the new process
  correct. A test that flushed and expected playback to continue uninterrupted now sees a gap.
- `close` waits, bounded: audio for the pipeline to reach end-of-stream (up to two seconds),
  video for the process to exit (up to five, then it is killed). A caller that relied on
  `close` returning immediately should measure again.
- `Drop` stops the stream if `close` was not called, and waits for its thread. Dropping a
  stream is no longer the same as leaking one.

## Removed modules

`rust_airplay_gstream::audio` and `rust_airplay_gstream::pipeline` were public and are
gone. `audio` exposed nothing; `pipeline` was reachable so that a test could call
`find_launcher`, and integration tests now use the hidden `testing` module, which runs the
same `Pipeline` the renderer runs. Nothing in either module was a supported way to play a
frame -- the `Renderer` trait was -- so a caller that used them was using this crate's
internals.

## Tests

The video fixture is now a synthetic 640x360 H.264 stream of 48 pictures instead of a
captured iPad mirror, so a test that hard-coded 1554x1080, thirty frames, or a frame's byte
size needs the new numbers; `testing::video_args` and `testing::feed` build the same pipeline
the renderer builds. `tools/make-fixtures.sh` regenerates the file byte for byte. The audio
fixture is still the captured 40-frame AAC-ELD stream.

```sh
cargo test                          # unit tests, no GStreamer runtime
cargo test --all-targets -- --ignored   # needs the runtime plugins
```
