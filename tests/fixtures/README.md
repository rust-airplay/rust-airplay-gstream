# Test fixtures

Two small binary fixtures are checked in so the tests can decode real-shaped input
without capturing anything at test time. Both are embedded with include_bytes!, so
they compile into the test binaries.

## mirror-first-frames.h264

A synthetic H.264 Annex-B video stream: a single elementary stream, no container
and no audio. It is generated from GStreamer's videotestsrc through openh264enc, so
it contains a test pattern only - no camera image, no screen capture, no personal
data.

| property | value |
| --- | --- |
| format | H.264 Annex-B, one elementary stream, no container, no audio |
| resolution | 640x360 |
| coded pictures | 48 (47 non-IDR slice NALs, type 1, and 1 IDR picture, type 5) |
| access unit delimiters | 48 (NAL type 9) |
| SPS / PPS | 1 each (NAL types 7 and 8), both emitted before the first slice |
| other NAL types | none - in particular no SEI (type 6) |
| file size | 15379 bytes |
| sha256 | f980f0f3b895944a3d4b3a90eb8ed685379cc74c8cb8b6420f0c9d6da850c49b |

Every NAL unit is introduced by a four-byte start code (00 00 00 01); the stream
contains no three-byte start codes. The SPS is a constrained-baseline 640x360 SPS
and the stream decodes completely: with both h264parse ! avdec_h264 and
decodebin, the decode produces 48 I420 frames, that is 16588800 bytes at
640 * 360 * 3 / 2 bytes per frame.

### Regenerating

```sh
tools/make-fixtures.sh            # regenerate, verify, and install the fixture
tools/make-fixtures.sh --check    # verify the checked-in fixture only
tools/make-fixtures.sh --force    # install even if the encoder output drifted
```

The script runs exactly this pipeline, which is the whole definition of the file:

```sh
gst-launch-1.0 -q videotestsrc num-buffers=48 pattern=ball \
    ! video/x-raw,width=640,height=360,framerate=30/1 \
    ! openh264enc \
    ! h264parse config-interval=-1 \
    ! video/x-h264,stream-format=byte-stream,alignment=au \
    ! filesink location=tests/fixtures/mirror-first-frames.h264
```

openh264enc is deterministic for this input: two independent runs produce the
sha256 above byte for byte. Regeneration needs GStreamer with the videotestsrc,
openh264enc (gst-plugins-bad), h264parse, avdec_h264 and fdsink elements on PATH.
The script also checks the resolution, the coded picture count, the file size and
both decoders, and it checks the audio fixture described below, so it can be used
in CI as a "the fixture is what it claims to be" gate.

## mirror-first-audio.eld

40 raw AAC-ELD payloads, each preceded by a four-byte little-endian length: 448
bytes for the first frame, then 39 payloads of 145 to 190 bytes. Parsing the
length prefixes consumes exactly 6823 bytes, so the file ends after the last
frame with no trailing data.

| property | value |
| --- | --- |
| format | raw AAC-ELD payloads, each with a 4-byte little-endian length prefix |
| frames | 40 |
| file size | 6823 bytes |
| sha256 | bb950cc82253803d5f0a0ab28792c6ed323f189dd6aa52410f17a0d553574aae |

Provenance: the payloads were captured from a real AirPlay session and are kept
because no AAC-ELD encoder is available to synthesize them - OpenH264 does not
encode audio and the AAC encoders that ship with GStreamer have no ELD mode - so
this file cannot currently be regenerated. It is data the receiver decodes, not
recorded user content: it holds only coded audio payloads, with no metadata,
filenames, or identifying information, and it is not part of the video privacy
issue that led to the synthetic replacement above. The file is never rewritten by
tools/make-fixtures.sh; the script only parses it and verifies its sha256.

## Licensing

Both fixtures belong to this project and are distributed under GPL-3.0-or-later,
the crate's license. The video fixture is output of this project's own generation
script, so it carries no third-party rights; the audio fixture is a short capture
of protocol data with no authorship interest attached.
