#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Regenerates tests/fixtures/mirror-first-frames.h264 and checks that it is still the
# stream this crate's tests expect.
#
# The fixture holds no captured picture: it is 48 frames of GStreamer's own video test
# source (the "ball" pattern), encoded by openh264enc into one H.264 Annex-B byte
# stream. It is synthetic on purpose. The file it replaces was a real capture of a
# desktop, which is somebody's screen, in a repository that is published.
#
# Usage:
#   tools/make-fixtures.sh           regenerate, verify, and install the fixture
#   tools/make-fixtures.sh --check   verify the checked-in files without writing
#   tools/make-fixtures.sh --force   install a regeneration whose sha256 differs
#                                    (a deliberate encoder upgrade)
#
# Requirements: gst-launch-1.0 with videotestsrc, openh264enc and h264parse (MSYS2:
# mingw-w64-ucrt-x86_64-gstreamer plus mingw-w64-ucrt-x86_64-gst-plugins-bad), awk,
# od, sha256sum. It runs in bash under MSYS2. Every path handed to GStreamer is
# relative to the repository root, because gst-launch-1.0 is a native Windows program
# and does not understand an MSYS2 path such as /d/Projects.

set -euo pipefail

readonly ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly FIXTURE_REL="tests/fixtures/mirror-first-frames.h264"
readonly ELD_REL="tests/fixtures/mirror-first-audio.eld"
readonly TEMP_REL="tests/fixtures/.mirror-first-frames.h264.tmp"

# What the stream is. Every number below is checked, not assumed.
readonly WIDTH=640
readonly HEIGHT=360
readonly CODED_PICTURES=48
readonly FRAME_BYTES=$(( WIDTH * HEIGHT * 3 / 2 ))
readonly GEOMETRY="640x360"

# The sha256 of the fixture this repository commits. Regenerating on the same
# GStreamer build reproduces it byte for byte; a different openh264 version may not,
# which is what --force is for.
readonly EXPECTED_SHA256="f980f0f3b895944a3d4b3a90eb8ed685379cc74c8cb8b6420f0c9d6da850c49b"

# The audio fixture's sha256. It is not regenerated: no AAC-ELD encoder is available
# to synthesize it, and its payloads carry no personal information (see
# tests/fixtures/README.md).
readonly EXPECTED_ELD_SHA256="bb950cc82253803d5f0a0ab28792c6ed323f189dd6aa52410f17a0d553574aae"
readonly ELD_FRAMES=40

usage() {
    sed -n '9,22p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

fail() {
    printf 'make-fixtures: %s\n' "$*" >&2
    exit 1
}

note() {
    printf '%s\n' "$*"
}

require_commands() {
    local tool
    for tool in gst-launch-1.0 awk od sha256sum; do
        command -v "$tool" >/dev/null 2>&1 || fail "$tool is not on PATH"
    done
    gst-inspect-1.0 openh264enc >/dev/null 2>&1 || fail \
        "the openh264enc element is not installed (MSYS2: mingw-w64-ucrt-x86_64-gst-plugins-bad)"
    gst-inspect-1.0 h264parse >/dev/null 2>&1 || fail \
        "the h264parse element is not installed (MSYS2: mingw-w64-ucrt-x86_64-gstreamer)"
}

# Encodes the fixture. This one command is the file; everything else here checks it.
encode() {
    gst-launch-1.0 -q \
        videotestsrc num-buffers="$CODED_PICTURES" pattern=ball \
        ! video/x-raw,width="$WIDTH",height="$HEIGHT",framerate=30/1 \
        ! openh264enc \
        ! h264parse config-interval=-1 \
        ! video/x-h264,stream-format=byte-stream,alignment=au \
        ! filesink location="$1"
}

# Walks the start codes and reports what the stream is made of.
#
# One line of space-separated numbers: coded pictures (type 1 or 5), SPS, PPS, access
# unit delimiters, SEI, and the distinct NAL types that are none of those ("-" for
# none). A constant negative SPS or PPS count means a parameter set was missing before
# the first slice.
nal_inventory() {
    od -An -v -tu1 "$1" | awk '
        { for (i = 1; i <= NF; i++) byte[++n] = $i + 0 }
        END {
            pictures = 0; sps = 0; pps = 0; aud = 0; sei = 0
            sps_at = 0; pps_at = 0; first_slice = 0; seen = ""
            i = 1
            while (i + 2 <= n) {
                if (byte[i] == 0 && byte[i + 1] == 0 \
                    && (byte[i + 2] == 1 || (i + 3 <= n && byte[i + 2] == 0 && byte[i + 3] == 1))) {
                    at = (byte[i + 2] == 1) ? i + 3 : i + 4
                    if (at > n) break
                    t = byte[at] % 32
                    if (t == 1 || t == 5) {
                        pictures++
                        if (first_slice == 0) first_slice = at
                    } else if (t == 7) {
                        sps++
                        if (sps_at == 0) sps_at = at
                    } else if (t == 8) {
                        pps++
                        if (pps_at == 0) pps_at = at
                    } else if (t == 9) {
                        aud++
                    } else if (t == 6) {
                        sei++
                    } else if (index(" " seen " ", " " t " ") == 0) {
                        seen = seen " " t
                    }
                    i = at + 1
                } else {
                    i++
                }
            }
            if (first_slice > 0 && (sps_at == 0 || pps_at == 0 || sps_at > first_slice || pps_at > first_slice)) {
                sps = -sps
                pps = -pps
            }
            other = (seen == "") ? "-" : substr(seen, 2)
            gsub(/ /, ",", other)
            printf "%d %d %d %d %d %s\n", pictures, sps, pps, aud, sei, other
        }'
}

# The geometry h264parse reads out of the SPS.
resolution() {
    local caps found
    caps=$(gst-launch-1.0 -v filesrc location="$1" \
        ! h264parse \
        ! fakesink 2>&1 || true)
    # Two pads print the same caps; the first match is the answer. The second sed
    # reads the whole stream, so neither one exits early into a broken pipe.
    found=$(printf '%s\n' "$caps" \
        | sed -n 's/.*width=(int)\([0-9][0-9]*\), height=(int)\([0-9][0-9]*\).*/\1x\2/p' \
        | sed -n '1p')
    printf '%s\n' "$found"
}

# How many I420 frames a decoder actually produced.
decoded_frames() {
    local rel="$1" element="$2" raw
    if ! gst-inspect-1.0 "$element" >/dev/null 2>&1; then
        printf 'unavailable\n'
        return 0
    fi
    # decodebin already contains a decoder; h264parse needs one bolted on.
    local chain
    if [ "$element" = "decodebin" ]; then
        chain="$element"
    else
        chain="$element ! avdec_h264"
    fi
    if ! raw=$(gst-launch-1.0 -q filesrc location="$rel" \
        ! $chain \
        ! videoconvert \
        ! video/x-raw,format=I420 \
        ! fdsink 2>/dev/null | wc -c); then
        fail "$element ! avdec_h264 failed on $rel"
    fi
    raw=$(printf '%s' "$raw" | tr -d '[:space:]')
    if [ -z "$raw" ] || [ "$raw" -eq 0 ]; then
        printf '0\n'
        return 0
    fi
    [ $(( raw % FRAME_BYTES )) -eq 0 ] || fail \
        "$element produced $raw bytes, which is not a whole number of $FRAME_BYTES-byte frames"
    printf '%d\n' $(( raw / FRAME_BYTES ))
}

# The audio fixture as length-prefixed frames: a four-byte little-endian length, then
# that many bytes. Printed as "frames bytes-consumed".
eld_frames() {
    od -An -v -tu1 "$1" | awk '
        { for (i = 1; i <= NF; i++) byte[++n] = $i + 0 }
        END {
            at = 1; frames = 0
            while (at + 3 <= n) {
                # "length" is a gawk built-in; use len.
                len = byte[at] + 256 * byte[at + 1] + 65536 * byte[at + 2] + 16777216 * byte[at + 3]
                at += 4
                if (len <= 0 || at + len - 1 > n) break
                at += len
                frames++
            }
            printf "%d %d\n", frames, at - 1
        }'
}

sha_of() {
    sha256sum "$1" | cut -d' ' -f1
}

report() {
    local pictures sps pps aud sei other res by_parse by_bin size sha
    read -r pictures sps pps aud sei other <<<"$(nal_inventory "$FIXTURE_REL")"
    res="$(resolution "$FIXTURE_REL")"
    [ -n "$res" ] || res="unknown"
    by_parse="$(decoded_frames "$FIXTURE_REL" h264parse)"
    by_bin="$(decoded_frames "$FIXTURE_REL" decodebin)"
    size=$(wc -c <"$FIXTURE_REL" | tr -d '[:space:]')
    sha="$(sha_of "$FIXTURE_REL")"

    note "fixture:         $FIXTURE_REL"
    note "resolution:      $res"
    note "coded pictures:  $pictures (SPS $sps, PPS $pps, access unit delimiters $aud, SEI $sei, other NAL types $other)"
    note "bytes:           $size"
    note "sha256:          $sha"
    note "decoded frames:  $by_parse via h264parse, $by_bin via decodebin"

    [ "$res" = "$GEOMETRY" ] || fail "the stream is $res, expected $GEOMETRY"
    [ "$pictures" -eq "$CODED_PICTURES" ] || fail \
        "the stream holds $pictures coded pictures, expected $CODED_PICTURES"
    [ "$sps" -ge 1 ] && [ "$pps" -ge 1 ] || fail "no SPS and PPS before the first slice"
    [ "$size" -lt 153600 ] || fail "the stream is $size bytes, over the 150 KiB budget"
    # One fewer than the coded pictures is the last frame still inside the decoder.
    [ "$by_parse" -ge $(( CODED_PICTURES - 1 )) ] || fail \
        "h264parse decoded $by_parse of $CODED_PICTURES pictures"
    [ "$by_bin" -ge $(( CODED_PICTURES - 1 )) ] || fail \
        "decodebin decoded $by_bin of $CODED_PICTURES pictures"
}

report_eld() {
    local frames consumed sha size
    read -r frames consumed <<<"$(eld_frames "$ELD_REL")"
    size=$(wc -c <"$ELD_REL" | tr -d '[:space:]')
    sha="$(sha_of "$ELD_REL")"
    note "eld fixture:     $ELD_REL"
    note "eld frames:      $frames ($consumed of $size bytes as 4-byte length prefixes)"
    note "eld sha256:      $sha"
    [ "$frames" -eq "$ELD_FRAMES" ] || fail \
        "the ELD fixture holds $frames frames, expected $ELD_FRAMES"
    [ "$consumed" -eq "$size" ] || fail \
        "the ELD fixture has $(( size - consumed )) trailing bytes"
    [ "$sha" = "$EXPECTED_ELD_SHA256" ] || fail "the ELD fixture changed to $sha"
}

main() {
    local mode="regenerate" sha
    if [ $# -gt 0 ]; then
        case "$1" in
        --check) mode="check" ;;
        --force) mode="force" ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
        esac
        shift
    fi
    [ $# -eq 0 ] || { usage >&2; exit 2; }

    cd "$ROOT"
    require_commands

    if [ "$mode" = "check" ]; then
        [ -f "$FIXTURE_REL" ] || fail "no fixture at $FIXTURE_REL; run tools/make-fixtures.sh"
        sha="$(sha_of "$FIXTURE_REL")"
        [ "$sha" = "$EXPECTED_SHA256" ] || fail \
            "$FIXTURE_REL is $sha, not the recorded $EXPECTED_SHA256"
    else
        encode "$TEMP_REL"
        sha="$(sha_of "$TEMP_REL")"
        if [ "$sha" = "$EXPECTED_SHA256" ]; then
            mv -f "$TEMP_REL" "$FIXTURE_REL"
        elif [ "$mode" = "force" ]; then
            mv -f "$TEMP_REL" "$FIXTURE_REL"
            note "warning: the encoder produced $sha, not the recorded $EXPECTED_SHA256" >&2
            note "warning: --force installed it; update EXPECTED_SHA256 and tests/fixtures/README.md" >&2
        else
            rm -f "$TEMP_REL"
            fail "the encoder produced $sha, not the recorded $EXPECTED_SHA256 (use --force to accept it)"
        fi
    fi

    report
    report_eld
}

main "$@"
