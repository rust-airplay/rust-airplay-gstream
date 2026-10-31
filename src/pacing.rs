// SPDX-License-Identifier: GPL-3.0-or-later
//! Keeping video frames on a cumulative timeline.
//!
//! A mirror stream is one key frame followed by frames that only describe what changed,
//! so a receiver that decodes faster than real time loses frames the rest of the stream
//! depends on. When the receiver does not pace itself it asks the backend to, and the wait
//! has to be scheduled against a deadline rather than measured per frame: a fixed
//! `sleep(interval)` pays the sleep granularity again on every frame, and a machine whose
//! timer rounds up by a millisecond loses a second every thousand frames.
//!
//! The grid is anchored once, at the first frame of a session, and each frame's deadline is
//! the previous deadline plus the interval. A frame that is a little late still wakes on the
//! same grid, so jitter does not accumulate. A frame that is a whole interval late is not
//! jitter, it is a stall, and the grid is re-anchored at the current time instead of the
//! backend trying to catch up with a burst.

use std::time::{Duration, Instant};

/// The state one stream's pacing needs.
#[derive(Debug, Default)]
pub(crate) struct Pacer {
    /// When the next frame belongs. `None` anchors the grid at the next call.
    next_due: Option<Instant>,
}

impl Pacer {
    /// Starts a stream's timeline afresh.
    pub(crate) fn reset(&mut self) {
        self.next_due = None;
    }

    /// Waits until this frame's deadline on the stream's timeline.
    pub(crate) fn wait(&mut self, interval: Duration) {
        let wait = self.step(Instant::now(), interval);
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    /// Records a frame at `now` and returns how long the caller should wait for it.
    ///
    /// Kept separate from [`Pacer::wait`] so the arithmetic can be tested against a clock
    /// that does not sleep: ten thousand frames is the interesting length, and a test that
    /// really waits ten thousand intervals is a test nobody runs.
    fn step(&mut self, now: Instant, interval: Duration) -> Duration {
        let due = self.next_due.unwrap_or(now);
        let wait = due.saturating_duration_since(now);
        // The moment this frame actually belongs at: its deadline, or now when it is late.
        // Interval is clamped by the caller and instants come from the clock, so an
        // addition cannot overflow a process lifetime.
        let wake = now + wait;
        let grid = due + interval;
        // Keep the grid when the frame is late by less than one interval; re-anchor when a
        // whole one has been missed, which is a stall rather than jitter.
        self.next_due = Some(if grid < wake { wake + interval } else { grid });
        wait
    }
}

#[cfg(test)]
mod tests {
    use super::Pacer;
    use std::time::{Duration, Instant};

    /// What ten thousand frames of a 30-frame-a-second mirror would take.
    const INTERVAL: Duration = Duration::from_millis(33);

    #[test]
    fn ten_thousand_frames_stay_on_one_grid() {
        // A sleep returns a little late on every platform; the timeline must not pay for it
        // again on the next frame.
        let start = Instant::now();
        let mut pacer = Pacer::default();
        let mut now = start;
        let mut worst = Duration::ZERO;
        for frame in 0..10_000u32 {
            let wait = pacer.step(now, INTERVAL);
            now = now + wait + Duration::from_micros(300);
            let ideal = start + INTERVAL * frame;
            let drift = now.saturating_duration_since(ideal);
            worst = worst.max(drift);
        }
        assert!(
            worst < INTERVAL,
            "the timeline drifted {worst:?}, which is more than one frame"
        );
    }

    #[test]
    fn a_stall_re_anchors_instead_of_bursting() {
        let start = Instant::now();
        let mut pacer = Pacer::default();
        let mut now = start;
        for _ in 0..10 {
            let wait = pacer.step(now, INTERVAL);
            now += wait;
        }
        // The process was descheduled for half a second: the frames that were due in that
        // time are not owed, and the next one is one interval from now.
        now += Duration::from_millis(500);
        let wait = pacer.step(now, INTERVAL);
        assert!(
            wait.is_zero(),
            "a stalled stream does not sleep to catch up"
        );
        let mut previous = now;
        let mut waited = Duration::ZERO;
        for _ in 0..5 {
            let wait = pacer.step(previous, INTERVAL);
            previous += wait;
            waited += wait;
        }
        // Five frames cost four intervals, not none: the stall is not repaid as a burst.
        assert!(
            waited >= INTERVAL * 4,
            "five frames cost only {waited:?}, which is a catch-up burst"
        );
    }
}
