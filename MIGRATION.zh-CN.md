# 迁移：从工作区独立出来

English: [MIGRATION.md](MIGRATION.md)。

这个 crate 原本是单一工作区里的 `crates/rust-airplay-gstreamer`，因为音频路径要链接 `libgstreamer` 而被排除在默认构建之外。现在它独立成仓库，包名为 `rust-airplay-gstream`。

## 搬过来的内容

```
src/          渲染器：lib.rs、audio.rs、pipeline.rs
tests/        解码、音频、运行时与预热测试，以及它们抓取的码流
tools/        eld_probe.c，原先跟着库放在一起的解码诊断工具
```

`mirror-first-frames.h264` 与 `mirror-first-audio.eld` 之所以重要，是因为被忽略的测试能验证真实解码：它们是真机会话的帧，一条"接受每个 buffer 却一帧没解"的管线过不了这一关。

## 现在的依赖

```toml
rust-airplay = { git = "https://github.com/rust-airplay/rust-airplay", branch = "main" }
```

仍然是工作区里那个库，只是从路径依赖改成 Git 依赖。没有分叉：所有源文件原样搬过来，唯一的变化是导入的库名（`rust_airplay_gstreamer` 变成 `rust_airplay_gstream`）。

不提交 `Cargo.lock`。在核心仓库推送之前，这里的 `cargo build` 会在解析依赖时失败——那是顺序问题，不是代码问题。

## 门禁

```sh
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo test -- --ignored     # 需要 GStreamer 运行时
```

构建需要 GStreamer 开发库，见 [README.zh-CN.md](README.zh-CN.md)。
