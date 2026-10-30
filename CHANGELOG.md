# Changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the version numbers follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-10-30

The renderer boundary did not change shape in core 0.3, so this release is this backend
being held to it. Most of what follows is a bug that a session could hit; the version is a
breaking change because the public surface is smaller than 0.2's.

### Fixed

- **The published crate could not run its own tests.** `Cargo.toml`'s `include` list left
  out `tests/fixtures/`, while three integration tests load their media with
  `include_bytes!`, so a `.crate` taken from the registry had test targets that could not
  compile. The fixtures and `tools/make-fixtures.sh` are now packaged, and CI asserts
  they are there before publishing.
- **A missing element was reported as a failed request.** A pipeline that refused because
  `avdec_h264` is not installed came back as `RenderError::Failed`, which tells the
  caller its client is broken when the machine is short; the audio side already said
  `Unavailable`. Both do now, including when the element is only named in
  `gst-launch-1.0`'s standard error.
- **`flush` dropped nothing.** Both streams returned `Ok(())` without touching the pipeline,
  so a client that flushed heard the frames it had already sent. Audio now goes back to
  `READY` and to `PLAYING` again, which clears every queue between `appsrc` and the sink
  and re-anchors the timeline; video replaces its decoder process, because a
  `gst-launch-1.0` pipeline has no way to make a decoder forget what it queued.
- **Writing a frame could block forever.** The write held the input mutex across a blocking
  `write_all`, so a pipeline that stopped reading hung the session and every other thread
  that wanted that mutex, including `Drop`. Writes now happen on their own thread behind a
  four-buffer queue, and a frame the writer does not take within two seconds is a refused
  frame. Waiting for room in that queue is bounded too, which it was not at first: a full
  queue means the writer thread is stuck in a write, so offering it another job was the
  same unbounded wait wearing a different name.
- **An audio push could block forever on a sink that stopped draining.** `appsrc` blocks
  once its queue reaches `max-bytes`, and nothing bounded that wait. The source no longer
  blocks, the queue is bounded from this side (64 KiB, about two seconds of AAC-ELD), and a
  frame that would cross the bound is a refused frame that says the pipeline is not
  draining.
- **A second warm session could share one pipe.** The renderer handed the kept pipeline to
  a stream without asking whether one still held it, and two writers on one pipe interleave
  two H.264 streams. A pipeline now records whether a stream is writing through it, a closed
  stream releases it, and a session that finds it taken ends the old pipeline rather than
  sharing it.
- **A runtime audio failure was reported as an environment problem.** A pipeline that
  started and then failed came back as `Unavailable` from `push`; it is `Failed`, which is
  what the boundary means by it.
- **Caps could disagree with the configuration they carried.** Both codings put a fixed
  44.1 kHz stereo `codec_data` into the caps, so another sample rate or channel count
  produced a description whose two halves disagreed. Such a configuration is refused with
  `Unavailable` instead.
- **A diagnostic could be thrown away for one bad byte.** The pipeline's output was read as
  UTF-8 and an invalid sequence discarded the whole message; it is decoded lossily now, so
  the readable part survives.
- **Closing and dropping a stream had no deadline.** `finish` and `Drop` waited for the
  child with no timeout; they now wait five and two seconds and kill past that. Audio
  `close` sends end-of-stream and waits for the pipeline to reach it, so the sound card's
  own queue is played instead of being cut off; `Drop` stops the pipeline thread and waits
  for it, which 0.2 documented but did not do.
- **One stderr log could be shared, and it was world-readable.** The temporary file was
  opened with `File::create` against a name built from the process id and a counter. It is now
  created exclusively -- two receivers starting at once get two files -- and 0600 where the
  platform has permissions, because a decoder can quote a stream's own parameters in an
  error. At most 64 KiB is read back, and `AIRPLAY_GST_KEEP_STDERR` is still how the path
  is kept for diagnosis.
- **HEVC ignored the configured chain.** `Options` had one parser and one decoder, so a
  renderer configured for H.265 ran whichever chain the sink setter happened to string
  together. Each coding now has its own chain.
- **The warm pipeline was torn down by the session that used it.** The renderer handed its
  only handle to the stream, so `close` on a warm stream dropped the last reference and
  killed the pipeline warming was meant to keep. The renderer and the stream now each hold
  one, and only a coding mismatch or a dead process starts a new one.
- **The video fixture was a real desktop.** The captured H.264 stream was a screen recording
  of the author's machine, packaged into the crate. It is now a synthetic 640x360 stream
  that `tools/make-fixtures.sh` regenerates byte for byte; the audio fixture is still
  captured, because no AAC-ELD encoder exists to synthesize one. The old file remains in
  this repository's history.

### Changed

- **Against core 0.3.** `rust-airplay = "0.3"`, and the streams implement its
  `VideoStream::pause` and `resume` -- which this backend leaves as the default no-ops,
  because the sink holds the last frame on its own and the renderer has nothing to freeze.
  `Volume::level` is the accessor now.
- **`Options` is a value with a chain per coding** rather than a set of single-element
  setters. [MIGRATION.md](MIGRATION.md) has the old-to-new mapping.
- **The sinks are checked when the renderer is built.** A fragment may not contain a chain
  separator or quoting, and its first token must name an installed element. Pasting a
  caller's string into a pipeline description is how an option becomes a second branch of the
  pipeline, and a `filesink` in that branch writes a session to disk.
- **No `gst-launch-1.0` is not a failure to build.** The renderer starts, audio plays, and
  only a video request reports `Unavailable`. `GstRenderer::program` returns
  `Option<&str>` to match.
- **The video pipeline is built as an argument list**, not as one string split on whitespace: a
  caps filter contains commas and spaces, and a token list is the only form that survives
  `Command` on both Windows and Unix.
- **Frames are handed to the writer by move, not by copy.** The frame arrives owned, so the
  per-frame copy of a coded frame is gone; a clone measured 1 us at 64 KiB and 220 us at
  1 MiB, which is small but was pure overhead.
- **A spawn failure no longer leaks the child.** If the writer thread cannot start, the
  process that was already spawned is killed and reaped before the error is returned.
- **Frames have an upper bound** (`MAX_VIDEO_FRAME_BYTES`) and so does pacing
  (`MAX_PACE_SECONDS`), so a client cannot ask for a sleep it never wakes from.

### Added

- **`cargo-deny` in CI**, with a licence allow-list, the crates.io source only, and three
  advisories recorded as accepted until the minimum supported Rust version reaches 1.88.
- **A release gate that runs what publishing needs**: a tag whose version matches
  `Cargo.toml`, a clean tree, formatting, clippy, docs, both test layers, the packaged
  fixture assertion, and `cargo publish --dry-run` before the real one.
- **The integration tests run in CI**, on Linux and Windows, with the plugins they need
  installed and `gst-inspect-1.0` assertions that they are. 0.2 ran only the unit tests.
- **[tools/install-gstreamer.sh](tools/install-gstreamer.sh)** installs those packages on
  Ubuntu. The image ships LLVM's `libunwind-<version>-dev`, which GNU `libunwind-dev` -- a
  dependency of `libgstreamer1.0-dev` -- conflicts with, and apt will not remove installed
  packages to satisfy a dependency: it reported the dependency as unsatisfiable and exited
  100. The script removes those packages first, and keeps a repair-and-retry for the same
  symptom from other causes.
- **`Chain`**, and the `AUDIO_SINKS`, `VIDEO_SINKS`, `MAX_PACE_SECONDS` and
  `MAX_VIDEO_FRAME_BYTES` constants are public.
- **[tests/throughput.rs](tests/throughput.rs)** prints the video path's frame rate, so a
  change to the pipeline, the writer or the hand-off is measured against the same stream
  rather than argued about: 480 pictures decode at about 2000 frames a second on the
  development machine, including the pipeline start.
- [MIGRATION.md](MIGRATION.md): what moved between 0.2 and 0.3.

### Removed

- **`pub mod audio` and `pub mod pipeline`.** Both were internals with public names;
  `audio` exposed nothing at all and `pipeline` only existed so a test could find the
  launcher. Integration tests now use the hidden `testing` module, which drives the same
  code the renderer does.
- **`Options::with_video_sink` no longer changes the decoder chain**, and
  `Options::with_video_parser`/`with_video_decoder` now take the coding they apply to.

## [0.2.0]

First release against the core's 0.2 line: audio in-process through `appsrc`, video as a
`gst-launch-1.0` subprocess, and the warm-pipeline option.

[0.3.0]: https://github.com/rust-airplay/rust-airplay-gstream/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/rust-airplay/rust-airplay-gstream/releases/tag/v0.2.0
