//! Adapting to the link: a delay-based controller, as video calls use. Each frame's feedback gives
//! its one-way delay (the client's receive time less the capture time; the two clocks' offset is in
//! it, and cancels out against the lowest delay seen lately). Delay above that baseline is queuing:
//! the link is carrying more than it can. Then the bitrate and the frame rate drop at once; while
//! the queue stays empty they climb back, slowly. A connection that backs up counts as congestion too.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub kbps: u32,
    pub fps: u32,
}

pub struct Adapt {
    max: Target,
    min: Target,
    now: Target,
    /// One-way delays (ms, with the clock offset) of the last few seconds, for the baseline.
    delays: VecDeque<(Instant, f64)>,
    /// Queuing (ms over the baseline) of the last half second.
    queuing: VecDeque<(Instant, f64)>,
    backed_up: bool,
    last_cut: Instant,
    last_step: Instant,
}

const BASELINE_WINDOW: Duration = Duration::from_secs(10);
const RECENT: Duration = Duration::from_millis(500);
/// Queuing that means the link is full, and queuing low enough to try more.
const CONGESTED_MS: f64 = 40.0;
const CLEAR_MS: f64 = 10.0;
const START_KBPS: u32 = 2500;

impl Adapt {
    pub fn new(max_kbps: u32, max_fps: u32) -> Self {
        let max = Target {
            kbps: max_kbps.max(300),
            fps: max_fps.max(5),
        };
        let long_ago = Instant::now() - Duration::from_secs(60);
        Self {
            max,
            min: Target {
                kbps: 250.min(max.kbps),
                fps: 5.min(max.fps),
            },
            // A moderate start that climbs while the link keeps up (the first frame is a keyframe, the
            // biggest there is).
            now: Target {
                kbps: START_KBPS.min(max.kbps),
                fps: max.fps,
            },
            delays: VecDeque::new(),
            queuing: VecDeque::new(),
            backed_up: false,
            last_cut: long_ago,
            last_step: long_ago,
        }
    }

    pub fn target(&self) -> Target {
        self.now
    }

    /// A frame's one-way delay in ms (client receive time less capture time, clocks unsynced).
    pub fn feedback(&mut self, delay_ms: f64, at: Instant) {
        self.delays.push_back((at, delay_ms));
        while self.delays.front().is_some_and(|(t, _)| at.duration_since(*t) > BASELINE_WINDOW) {
            self.delays.pop_front();
        }
        let base = self.delays.iter().map(|(_, d)| *d).fold(f64::INFINITY, f64::min);
        self.queuing.push_back((at, (delay_ms - base).max(0.0)));
        while self.queuing.front().is_some_and(|(t, _)| at.duration_since(*t) > RECENT) {
            self.queuing.pop_front();
        }
    }

    /// The connection couldn't take what was sent (its buffer is full).
    pub fn backed_up(&mut self) {
        self.backed_up = true;
    }

    /// Queuing now: the median of the last half second (ms).
    pub fn queuing_ms(&self) -> f64 {
        let mut v: Vec<f64> = self.queuing.iter().map(|(_, q)| *q).collect();
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    /// Called a few times a second: a new target when it should change.
    pub fn tick(&mut self, at: Instant) -> Option<Target> {
        let q = self.queuing_ms();
        let before = self.now;
        if (self.backed_up || q > CONGESTED_MS) && at.duration_since(self.last_cut) > Duration::from_millis(300) {
            // Congested: down at once (and again if it stays so).
            self.now.kbps = ((self.now.kbps as f64 * 0.7) as u32).max(self.min.kbps);
            self.now.fps = ((self.now.fps as f64 * 0.75) as u32).max(self.min.fps);
            self.last_cut = at;
        } else if q < CLEAR_MS
            && !self.backed_up
            && at.duration_since(self.last_cut) > Duration::from_secs(1)
            && at.duration_since(self.last_step) > Duration::from_millis(250)
        {
            // Clear: back up a little at a time.
            self.now.kbps = ((self.now.kbps as f64 * 1.08) as u32 + 50).min(self.max.kbps);
            self.now.fps = (self.now.fps + 3).min(self.max.fps);
            self.last_step = at;
        }
        self.backed_up = false;
        (self.now != before).then_some(self.now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `ms` of one-way delay (with a clock offset) for `secs`, ticking as the server does; the
    /// lowest target it reached.
    fn run(a: &mut Adapt, t: &mut Instant, secs: f64, delay: impl Fn(f64) -> f64) -> Target {
        let steps = (secs * 60.0) as usize;
        let mut low = a.target();
        for i in 0..steps {
            *t += Duration::from_millis(16);
            a.feedback(1_000_000.0 + delay(i as f64 / 60.0), *t);
            if i % 15 == 0 {
                a.tick(*t);
                if a.target().kbps < low.kbps {
                    low = a.target();
                }
            }
        }
        low
    }

    #[test]
    fn backs_off_when_delay_builds_and_climbs_back_when_clear() {
        let mut a = Adapt::new(8000, 60);
        let mut t = Instant::now();
        run(&mut a, &mut t, 20.0, |_| 20.0);
        assert_eq!(a.target(), Target { kbps: 8000, fps: 60 }, "a clear link climbs to the top and stays");
        // The link fills: queuing grows by 30 ms a second.
        run(&mut a, &mut t, 3.0, |s| 20.0 + s * 30.0);
        assert!(a.target().kbps < 3000 && a.target().fps < 30, "it backed off: {:?}", a.target());
        // Clear again (the queue drains for a moment first): it climbs back, slowly.
        let low = run(&mut a, &mut t, 3.0, |_| 20.0);
        let mid = a.target();
        assert!(mid.kbps > low.kbps && mid.kbps < 8000, "climbing: {mid:?}");
        run(&mut a, &mut t, 30.0, |_| 20.0);
        assert_eq!(a.target(), Target { kbps: 8000, fps: 60 });
    }

    #[test]
    fn a_backed_up_connection_is_congestion() {
        let mut a = Adapt::new(8000, 60);
        let t = Instant::now() + Duration::from_secs(1);
        a.backed_up();
        assert_eq!(a.tick(t), Some(Target { kbps: 1750, fps: 45 }));
    }

    #[test]
    fn never_below_the_floor() {
        let mut a = Adapt::new(8000, 60);
        let mut t = Instant::now();
        for _ in 0..100 {
            t += Duration::from_millis(400);
            a.backed_up();
            a.tick(t);
        }
        assert_eq!(a.target(), Target { kbps: 250, fps: 5 });
    }
}
