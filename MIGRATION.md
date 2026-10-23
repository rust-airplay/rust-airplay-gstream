# Migration: out of the workspace

中文见 [MIGRATION.zh-CN.md](MIGRATION.zh-CN.md)。

This crate was `crates/rust-airplay-gstreamer` inside a single workspace, excluded from it
because its audio path links `libgstreamer`. It is a repository of its own now, and the
package is called `rust-airplay-gstream`.

## What moved here

```
src/          the renderer: lib.rs, audio.rs, pipeline.rs
tests/        decoding, audio, runtime and warming tests, with their captured fixtures
tools/        eld_probe.c, a decoder diagnostic that used to live with the library
```

The fixtures (`mirror-first-frames.h264`, `mirror-first-audio.eld`) are the reason the
ignored tests can check a real decode: they are frames from a device session, and a pipeline
that accepts every buffer while decoding none cannot pass them.

## What it depends on now

```toml
rust-airplay = { git = "https://github.com/rust-airplay/rust-airplay", branch = "main" }
```

The library is the same crate the workspace had, reached through Git instead of a path.
Nothing else was forked: every source file moved unchanged, except for the name of the
library it imports (`rust_airplay_gstreamer` became `rust_airplay_gstream`).

`Cargo.lock` is not committed. Until the core repository is pushed, `cargo build` here fails
while resolving the dependency; that is the ordering, not the code.

## Gates

```sh
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo test -- --ignored     # needs a GStreamer runtime
```

Building needs the GStreamer development files; see [README.md](README.md).
