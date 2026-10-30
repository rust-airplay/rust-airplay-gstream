# 从 0.2 迁移到 0.3

英文版：[MIGRATION.md](MIGRATION.md)。

0.3 对接 `rust-airplay 0.3`。渲染边界的形状没有变，所以下面的工作几乎全在本后端：更小的公开面、
构造期校验的 sink，以及能说明「哪一侧能力不足」的错误。为什么这么改见 [CHANGELOG.md](CHANGELOG.md)，
本页只讲机械映射。

## 依赖

```toml
rust-airplay = "0.3"
rust-airplay-gstream = "0.3"
```

把渲染器接进去的那一侧也必须一起升到 0.3——0.2 的核配 0.3 的后端等于两个不同的 `Renderer` trait，
报错会出现在 `Config::new` 上的类型不匹配。

## 流

`AudioStream` 没有变。`VideoStream` 新增 `pause` 与 `resume`，两者都有默认实现，
不关心它们的实现方照旧编译。本后端就保持默认：sink 自己会停在最后一帧，后端没有需要冻结的东西。

`AudioFrame::timestamp` 是采样数，不是纳秒；`VideoFrame::timestamp` 本后端根本不用——子进程
吃的是字节流，而字节流没有时间戳。如果你写了「推帧前先换算时间戳」的代码，删掉那段换算，否则码流
会以错误的速度播放。

`Volume` 同时有 `level` 和 `fraction`，所以 `volume.fraction()` 仍能编译；0.3 的文档
口径是 `level`，两者读同一个字段。

## `Options`

旧类型是一组「单一元素」的 setter，其中一个 setter 为所有编码设同一条链。现在它是一个带「每种编码
一条链」的值。

| 0.2 | 0.3 |
| --- | --- |
| `Options::default()` | `Options::default()`——仍然读环境变量 |
| `with_audio_sink(s)` | `with_audio_sink(s)` |
| `with_video_sink(s)` | `with_video_sink(s)`——不再改解码链 |
| `with_video_parser(p)` | `with_video_parser(codec, p)` |
| `with_video_decoder(d)` | `with_video_decoder(codec, d)` |
| 无 | `with_video_chain(codec, parser, decoder)` |
| `with_warm_video()` | `with_warm_video(bool)` |

另有 `Options::new(audio_sink, video_sink)`，供两个 sink 都显式命名、且不想查环境变量的调用方——
0.3 里这是字面意思：预热默认关闭，只有 `with_warm_video` 才会打开，而 0.2 的等价写法仍会读
`AIRPLAY_GST_VIDEO_WARM`。`Options::chain(codec)` 返回某种编码将要使用的那条 `Chain`。

```rust
use rust_airplay::{VideoCodec, Volume};
use rust_airplay_gstream::{GstRenderer, Options};

let options = Options::new("autoaudiosink", "autovideosink")
    .with_video_chain(VideoCodec::H265, "h265parse", "avdec_h265")
    .with_warm_video(true);
let renderer = GstRenderer::with_options(options)?;
```

## sink

sink 是「元素 + 属性」的一段片段——`fakesink`，或 `filesink location=out.raw`——并且在构造渲染器
时就校验。片段不能含 `!`，元素之后的每个 token 必须形如 `name=value`。不合规的片段会让
`GstRenderer::new`、`with_sinks` 或 `with_options` 返回错误，而不是等到某次会话打开流时
才失败：

```rust
// 0.2：构造成功，等会话开流时才失败。
// 0.3：
assert!(GstRenderer::with_sinks("fakesink", "no-such-sink").is_err());
```

原因是 sink 片段会被拼进管线描述，如果调用方可控，它就能加一条分支——例如把镜像会话写成磁盘上的
文件——而不是选择一个输出。

## 渲染器

- `program()` 返回 `Option<&str>`。`gst-launch-1.0` 只用于视频，所以没有它的机器也能构造出
  可以放音频的渲染器，只有视频请求会报 `RenderError::Unavailable`。调用方需要在请求视频之前就知道
  的话，用 `find_launcher()`（返回 `Result<String, RenderError>`）。
- `video_is_warm()`、`video_description(codec)`、`volume()`、`audio_sink()`、
  `video_sink()` 与以前一致。
- `with_warm_video` 仍在构造期预热；在没有 `gst-launch-1.0` 的机器上请求预热，构造函数会直接
  返回 `Unavailable`，而不是给出一个做不到的承诺。
- 一条 warm 管线一次只交给一条流。上一条流还握着它时来了新会话，就结束旧管线另起一条，而不是共用
  一根管道——两个写者会把两路 H.264 交织在一起。

## 错误

`RenderError::Unavailable` 表示这台机器做不到：没有 launcher、sink 或解码器未安装、没有声卡，
或者是一个本后端没有对应描述的音频配置。`Failed` 表示管线接受了请求然后失败。0.2 把「视频元素
缺失」报成 `Failed`，现在这类会话拿到的是 `Unavailable`；而启动之后才失败的音频管线报 `Failed`
而不是 `Unavailable`，按两者分支的代码可能需要换一个臂。

44.1 kHz 立体声以外的音频配置会被拒绝。两种编码的 `codec_data` 里都写死了这一配置，用别的采样率
或声道数拼出来的 caps 前后矛盾、解码器读不对；核心 0.3 对本 crate 支持的所有格式都用 44.1 kHz
立体声，所以正常的 0.3 会话不会碰到它。

## 流的生命周期

- `flush` 现在真的会丢弃尚未播出的内容。音频走 `READY` 再回到 `PLAYING`，清空队列并把
  时间线重新锚定在下一帧；视频停掉解码进程，再用同样的参数起一个。因此视频的 flush 要付一次管线
  启动的代价，客户端随后发来的关键帧才是让新进程正确的原因。以前 flush 后预期播放不中断的测试，现在
  会看到一段空隙。
- `close` 仍然会等，只是有了上界：音频等管线到 end-of-stream（最多两秒），视频等进程退出（最多
  五秒，之后杀掉）。依赖「`close` 立即返回」的调用方应该重新测一下。
- `Drop` 在没调用过 `close` 时停掉流，并等自己的线程结束。丢弃一条流不再等于泄漏一条流。

## 被移除的模块

`rust_airplay_gstream::audio` 与 `rust_airplay_gstream::pipeline` 原本是公开的，现在没了。
`audio` 什么都没暴露；`pipeline` 之所以可见，只是为了测试能调用 `find_launcher`，现在集成
测试改用隐藏的 `testing` 模块，跑的是渲染器自己那个 `Pipeline`。这两个模块原本都不是「播放一帧」
的受支持方式——那是 `Renderer` trait——用到它们就是在用本 crate 的内部实现。

## 测试

视频 fixture 现在是合成的 640x360 H.264 流、48 张图像，不再是采集来的 iPad 镜像，所以写死 1554x1080、
三十帧或单帧字节数的测试要用新数字；`testing::video_args` 与 `testing::feed` 构建的就是渲染器
构建的那条管线。`tools/make-fixtures.sh` 能逐字节重新生成该文件。音频 fixture 仍是采集来的 40 帧
AAC-ELD 流。

```sh
cargo test                              # 单元测试，不需要 GStreamer 运行时
cargo test --all-targets -- --ignored    # 需要运行时插件
```
