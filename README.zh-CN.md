# rust-airplay-gstream

[`rust-airplay`](https://github.com/rust-airplay/rust-airplay) 的 GStreamer 后端。

`rust-airplay` 接收 AirPlay 会话、解密，并把编码后的帧交出来。这个 crate 负责解码与播放：音频走进程内链接的 GStreamer 管线，视频走 `gst-launch-1.0` 子进程。它实现核心的 `Renderer` trait，所以程序依赖核心拿协议、依赖这个 crate 拿播放。

[`docs/ARCHITECTURE.zh-CN.md`](docs/ARCHITECTURE.zh-CN.md) 解释了两种媒体为何采用不同形态、一帧数据会经历什么，以及冲刷在这里能做到什么、做不到什么。

## 依赖

**构建**需要 GStreamer 开发文件，通过 `pkg-config` 找到：`gstreamer-1.0` 与 `gstreamer-app-1.0`。

**运行**需要 GStreamer 运行时与插件，视频路径还需要 `PATH` 上的 `gst-launch-1.0`：

| 用途 | 元素或程序 |
| --- | --- |
| 视频 | `gst-launch-1.0`、`h264parse` 或 `h265parse`、`avdec_h264` 或 `avdec_h265`（或 Direct3D11 那一对）、`videoconvert`、一个 sink |
| 音频 | `avdec_aac` 或 `avdec_alac`、`audioconvert`、`audioresample`、一个 sink |

Windows 上 MSYS2 UCRT64 两者都有，把 `C:\msys64\ucrt64\bin` 放进 `PATH`。Debian 与 Ubuntu 上，`libgstreamer1.0-dev` 与 `libgstreamer-plugins-base1.0-dev` 用于构建，上面这些元素所在的插件包用于运行。

## 使用

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

这个 crate 还没有发布到 crates.io：它通过 git 依赖核心，在核心发布之前 `publish` 为 false。

## Options

调用方能选的一切都是 `Options` 的字段：

| 字段 | 含义 | 默认值 |
| --- | --- | --- |
| `audio_sink` | GStreamer 音频 sink | 依次找 `wasapisink`、`autoaudiosink`、`osssink`、`alsasink`、`pulsesink`、`fakesink` 中第一个存在的 |
| `video_sink` | GStreamer 视频 sink | 依次找 `d3d11videosink`、`autovideosink`、`glimagesink`、`fakesink` 中第一个存在的 |
| `video_parser` | 镜像管线使用的 parser | `h264parse`，HEVC 时为 `h265parse` |
| `video_decoder` | 解码器及其所需转换器 | 随 sink 一起选择 |
| `warm_video` | 视频管线随接收端一起启动 | 关闭 |

每个默认值都与对应环境变量所产生的值一致，所以 `Options::default()` 的行为与 `GstRenderer::new` 完全相同。`Options::with_video_sink` 同时替换 sink 与解码链，因为 `d3d11h264dec` 解出的内存只有 `d3d11videosink` 能接收；只换其中一个，硬件 sink 就会被软件解码器喂数据。

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `AIRPLAY_GST_LAUNCH` | 要运行的 `gst-launch-1.0` 可执行文件 |
| `AIRPLAY_GST_AUDIO_SINK` | 音频 sink |
| `AIRPLAY_GST_VIDEO_SINK` | 视频 sink |
| `AIRPLAY_GST_VIDEO_WARM` | 设为 `1` 时视频管线随接收端启动 |
| `AIRPLAY_GST_KEEP_STDERR` | 保留子进程的错误输出而不是删除，并打印其路径 |

## 一条流会经历什么

- **视频**按推入顺序写进管线。子进程拿到的是字节流，所以时间戳不会进入其中；节流发生在本侧，由核心要求时执行。
- **音频**保留时间戳：每帧是一个 `appsrc` 缓冲区，其呈现时间来自客户端的采样计数，sink 按采集速率播放。
- **音量**是 sink 之前 `volume` 元素上的线性增益，应用到正在播放的流，并为下一条流记住。

## 限制

- `flush` 在两条流上都是空操作：管道没有带内冲刷，`appsrc` 管线也无法丢弃已推入的缓冲区。视频解码器会在下一个关键帧重新同步，核心的队列也在同一处重启。
- 没有 HLS 与录制路径；它们属于核心，而核心没有实现。
- 视频路径每条流运行一个 `gst-launch-1.0`（或整个接收端共用一个热管线），所以观看者无法在管线内拖动；它是直播流。

## 测试

```sh
cargo test
cargo test -- --ignored
```

被忽略的测试需要 GStreamer 运行时与 `gst-launch-1.0`。它们用真实会话里抓下来的帧做解码，这正是它们的价值所在：一条管线可以接受每个缓冲区却一个都不解码，只有解码输出能回答这个问题。

## 许可证

GPL-3.0-or-later。见 [LICENSE](LICENSE)。

English: [README.md](README.md)。
