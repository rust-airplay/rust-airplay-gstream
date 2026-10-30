// SPDX-License-Identifier: GPL-3.0-or-later
//! Turning a client's sample counter into a presentation time.

/// Samples between two RTP timestamps, counting the 32-bit wrap.
///
/// The counters are the client's: they advance by the frame's own sample count and
/// wrap at 2^32. A receiver that subtracted them as 64-bit values would put a frame
/// after the wrap at a time a day ahead of the stream.
pub(crate) fn elapsed_samples(stamp: u32, first: u32) -> u64 {
    u64::from(stamp.wrapping_sub(first))
}

/// How long after a stream's first frame a frame with `samples` elapsed samples is due.
///
/// Kept separate from the pipeline so the arithmetic can be tested without one: a
/// presentation time that is wrong by a factor of the sample rate turns into audio that
/// plays at the wrong speed, and that is worth a test that does not need a sound card.
pub(crate) fn relative_nanos(samples: u64, rate: u32) -> u64 {
    let rate = u64::from(rate.max(1));
    (samples / rate).saturating_mul(1_000_000_000)
        + (samples % rate).saturating_mul(1_000_000_000) / rate
}

#[cfg(test)]
mod tests {
    use super::{elapsed_samples, relative_nanos};

    #[test]
    fn a_client_counter_that_wraps_still_measures_elapsed_audio() {
        // The counter is 32 bits. A stream that runs for a day reaches the wrap, and a
        // receiver that subtracted 64-bit values would put the next frame about a day
        // ahead.
        assert_eq!(elapsed_samples(500, 0), 500);
        // 0x100 after 0xFFFFFF00 is 0x200 samples later, not a negative span.
        assert_eq!(elapsed_samples(0x100, 0xFFFF_FF00), 0x200);
        assert_eq!(elapsed_samples(0, 0xFFFF_FFFF), 1);
        // And the two frames are still ten milliseconds apart.
        assert!(relative_nanos(elapsed_samples(0x100, 0xFFFF_FF00), 44_100) < 12_000_000);
    }

    #[test]
    fn one_frame_of_forty_four_one_kilohertz_audio_is_ten_milliseconds() {
        // What a device sends at 44.1 kHz is 480 samples a frame, which is 10.88 ms.
        assert_eq!(relative_nanos(480, 44_100), 10_884_353);
        // And a whole second is a whole second, at both rates this crate opens.
        assert_eq!(relative_nanos(44_100, 44_100), 1_000_000_000);
        assert_eq!(relative_nanos(48_000, 48_000), 1_000_000_000);
    }

    #[test]
    fn the_presentation_clock_does_not_drift() {
        // The time of a frame is computed from its timestamp, never accumulated from the
        // frame before, so a rounding error cannot add up.
        for frames in [1u64, 30, 1_000, 5_515] {
            let samples = frames * 480;
            let expected = samples * 1_000_000_000 / 44_100;
            let computed = relative_nanos(samples, 44_100);
            assert!(
                computed.abs_diff(expected) < 1,
                "{frames} frames gave {computed}, expected {expected}"
            );
        }
    }

    #[test]
    fn later_frames_are_always_later() {
        let mut previous = relative_nanos(0, 44_100);
        for frames in 1..10u64 {
            let now = relative_nanos(frames * 480, 44_100);
            assert!(now > previous, "frame {frames} did not advance");
            previous = now;
        }
    }
}
