# rust-airplay-gstream

[![CI](https://github.com/rust-airplay/rust-airplay-gstream/actions/workflows/ci.yml/badge.svg)](https://github.com/rust-airplay/rust-airplay-gstream/actions/workflows/ci.yml)

[`rust-airplay`](https://github.com/rust-airplay/rust-airplay) 的 GStreamer 后端。

`rust-airplay` 接收 AirPlay 会话、解密，并把编码后的帧交出来。这个 crate 负责解码与播放：音频走进程内链接的 GStreamer 管线，视频走 `gst-launch-1.0` 子进程。它实现核心的 `Renderer` trait，所以程序依赖核心拿协议、依赖这个 crate 拿播放。

[`docs/ARCHITECTURE.zh-CN.md`](docs/ARCHITECTURE.zh-CN.md) 解释了两种媒体为何采用不同形态、一帧数据会经历什么，以及冲刷在这里能做到什么、做不到什么。

[CHANGELOG.md](CHANGELOG.md) 记录了 0.3 改了什么，[MIGRATION.zh-CN.md](MIGRATION.zh-CN.md) 把基于 0.2 的构建映射到它上面。

## 依赖

**构建**需要 GStreamer 开发文件，通过 `pkg-config` 找到：`gstreamer-1.0` 与 `gstreamer-app-1.0`。

**运行**需要 GStreamer 运行时与插件：

| 用途 | 元素或程序 |
| --- | --- |
| 视频 | `gst-launch-1.0`、`h264parse` 或 `h265parse`、`avdec_h264` 或 `avdec_h265`、一个 sink |
| 音频 | `avdec_aac` 或 `avdec_alac`、`audioconvert`、`audioresample`、一个 sink |

`avdec_*` 解码器来自 `gst-libav`。只播音频的机器不需要 `gst-launch-1.0`：渲染器照常构建、音频照常播放，只有视频请求会报告不可用。

Windows 上 MSYS2 UCRT64 两者都有，把 `C:\msys64\ucrt64\bin` 放进 `PATH`。Debian 与 Ubuntu 上，`libgstreamer1.0-dev` 与 `libgstreamer-plugins-base1.0-dev` 用于构建，上面这些元素所在的插件包用于运行。

## 使用

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

两个 crate 都已发布到 crates.io。`rust-airplay-gstream` 跟随核心的 0.3 系列，`Cargo.lock` 会把两者解析到同一组版本。依赖下限是 `rust-airplay` 0.3.1：0.3.0 的接收端把节拍委托给后端，而且镜像队列溢出时只丢最旧一帧；这两点都在 0.3.1 修好了。

## Options

调用方能选的一切都在 `Options` 里，且每个默认值都与对应环境变量产生的值一致，所以 `Options::default` 的行为与 `GstRenderer::new` 完全相同：

| 方法 | 含义 | 默认值 |
| --- | --- | --- |
| `Options::new(audio, video)` | 直接给出两个 sink | -- |
| `.with_audio_sink(sink)` | GStreamer 音频 sink | 依次找 `wasapisink`、`autoaudiosink`、`osssink`、`alsasink`、`pulsesink`、`fakesink` 中第一个存在的 |
| `.with_video_sink(sink)` | GStreamer 视频 sink | 依次找 `d3d11videosink`、`autovideosink`、`glimagesink`、`fakesink` 中第一个存在的 |
| `.with_video_chain(codec, parser, decoder)` | H.264 或 H.265 的 parser 与解码器 | `h264parse` + `avdec_h264`，`h265parse` + `avdec_h265` |
| `.with_warm_video(bool)` | 在会话之间保留视频管线 | 关闭 |
| `.with_pacing(bool)` / `.without_pacing()` | 按累计时间线给视频帧节拍 | 开启 |

一个 sink 是一个元素，外加可选的属性——`fakesink`，或 `filesink location=out.raw`。它在构建渲染器时就被检查：元素必须存在，且这个片段必须是单个元素而不是一条管线，所以 sink 无法把 `!` 分隔的元素或另一个 sink 拉进本后端构建的管线。

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

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `AIRPLAY_GST_LAUNCH` | 要运行的 `gst-launch-1.0` 可执行文件 |
| `AIRPLAY_GST_AUDIO_SINK` | 音频 sink，按片段写法 |
| `AIRPLAY_GST_VIDEO_SINK` | 视频 sink，按片段写法 |
| `AIRPLAY_GST_VIDEO_WARM` | 设为 `1`（或除 `0`/`false`/`no`/`off` 之外的值）时在会话之间保留视频管线 |
| `AIRPLAY_GST_KEEP_STDERR` | 保留子进程的输出而不是删除，并记录其路径 |

`Options` 在构造时读取这些变量，所以想在代码里选择的程序应使用 `Options::new` 与各个 `with_*` 方法。

## 一条流会经历什么

- **视频**按推入顺序写进子进程。字节流不携带时间戳；`rust-airplay` 0.3.1 起由接收端自己节拍（或者全速推、让管道背压决定速率），不再调用 `VideoStream::set_pace`。这个钩子留给直接驱动 trait 的调用方，本后端按 trait 文档的契约实现：等待排在累计截止时间上，某次 sleep 多睡了一点不会把后面每帧推后；间隔为 0 表示「不要等」而不是「睡 0 秒」；落后超过一个间隔时在「现在」重锚定，而不是用爆发补；自己按 sink 时钟调度可以用 `Options::without_pacing()` 关掉。
- **音频**保留时序：每帧是一个 `appsrc` 缓冲区，其呈现时间是距会话首帧的采样偏移，按会话采样率换算。
- **音量**是 sink 之前 `volume` 元素上的线性增益，应用到正在播放的流，并为下一条流记住。
- **flush** 丢弃已排队的内容：音频先回 `READY` 再重新启动；视频直接替换解码进程，这是让解码器忘掉队列的唯一办法。冲刷之后客户端发来的关键帧，才让新解码器正确而不只是空白。

## 失败

`RenderError::Unavailable` 表示这台机器做不到：没有 `gst-launch-1.0`、sink 或解码元素未安装、没有 GStreamer 运行时。`RenderError::Failed` 表示请求没能完成：程序拒绝了描述、管线中途停止。接收端把两种都报出来并继续服务；一个在内部自动重试的后端只是在掩盖自己的失败。

## 测试

```sh
cargo test                          # 单元测试，不需要 GStreamer
cargo test -- --ignored             # 需要 GStreamer 运行时与 gst-launch-1.0
```

集成测试解码 fixture 并数帧：一条管线可以接受每个缓冲区却一个都不解码，只有解码输出能回答这个问题。视频 fixture 是合成的——`videotestsrc` 经 `openh264enc`，640x360，48 张图——因此不含任何截屏内容，[tools/make-fixtures.sh](tools/make-fixtures.sh) 能逐字节重新生成它。音频 fixture 是真实会话里的 40 个裸 AAC-ELD 载荷，之所以保留，是因为没有能合成它们的 AAC-ELD 编码器。[`tests/fixtures/README.md`](tests/fixtures/README.md) 记录了每个文件的来历与复现方式。

## 发布

发布是一个由 `vX.Y.Z` 标签驱动的工作流，标签版本必须与 `Cargo.toml` 一致。它会拒绝脏工作树、跑完整套测试（包括被忽略的那些）、检查打包出的 `.crate` 里含有 fixtures，并在真正上传前先 dry-run 一次，最后用仓库 secret `CARGO_REGISTRY_TOKEN` 发布。请先发布它依赖的核心版本：打包本 crate 时 Cargo 会从 crates.io 解析 `rust-airplay = "0.3"`。

## 许可证

GPL-3.0-or-later。见 [LICENSE](LICENSE)。

English: [README.md](README.md)。
