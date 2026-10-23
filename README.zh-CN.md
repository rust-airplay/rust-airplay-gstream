# rust-airplay-gstream

[`rust-airplay`](https://github.com/rust-airplay/rust-airplay) 的 GStreamer 后端。

`rust-airplay` 接收 AirPlay 会话、解密，并交付编码后的帧；这个 crate 负责解码与播放：音频通过链接的 GStreamer 管线，视频通过 `gst-launch-1.0` 子进程。它实现 `Renderer` trait，所以一个程序用核心库做协议、用这个 crate 做播放。

## 依赖

构建需要 GStreamer 开发库，由 `pkg-config` 查找：`gstreamer-1.0` 与 `gstreamer-app-1.0`。运行需要 GStreamer 运行时，且下列元素可用：

| 用途 | 元素 |
| --- | --- |
| 视频 | `h264parse` 或 `h265parse`、`avdec_h264` 或 `avdec_h265`（或 Direct3D11 那一对）、`videoconvert`、一个 sink |
| 音频 | `avdec_aac` 或 `avdec_alac`、`audioconvert`、`audioresample`、一个 sink |

Windows 上 MSYS2 UCRT64 两者都提供，把 `C:\msys64\ucrt64\bin` 加进 `PATH`。Debian 与 Ubuntu 上，`libgstreamer1.0-dev` 与 `libgstreamer-plugins-base1.0-dev` 用于构建，上述元素所在的插件包用于运行。

## 用法

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

调用方可选项都在 `Options` 里：两个 sink、视频 parser 与 decoder、是否预热视频管线。每项默认值都来自环境变量，因此 `Options::default()` 的行为与 `GstRenderer::new` 完全一致。`Options::with_video_sink` 会让解码器跟着 sink 一起换——`d3d11h264dec` 解出的数据只能交给 `d3d11videosink`。

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `AIRPLAY_GST_LAUNCH` | 指定要运行的 `gst-launch-1.0` |
| `AIRPLAY_GST_AUDIO_SINK` | 音频 sink，Windows 上为 `wasapisink` |
| `AIRPLAY_GST_VIDEO_SINK` | 视频 sink，默认 `d3d11videosink`（存在时），否则 `autovideosink` |
| `AIRPLAY_GST_VIDEO_WARM` | 置 `1` 时视频管线随接收端一起启动 |
| `AIRPLAY_GST_KEEP_STDERR` | 保留子进程错误输出，不重定向到文件 |

## 测试

```sh
cargo test
cargo test -- --ignored
```

被忽略的测试需要 GStreamer 运行时。它们解码真机会话抓取的帧——这正是它们有用的原因：一条管线可以接受每一个 buffer 却一帧都没解出来，只有解码输出能回答这个问题。

## 许可证

GPL-3.0-or-later。见 [LICENSE](LICENSE)。

English: [README.md](README.md)。
