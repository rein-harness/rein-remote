//! Adapting to the link: a delay-based controller, as video calls use. Each frame's feedback gives
//! its one-way delay (the client's receive time less the capture time; the two clocks' offset is in
//! it, and cancels out against the lowest delay seen lately). Delay above that baseline is queuing:
//! the link is carrying more than it can. A connection that backs up counts as congestion too.
//!
//! - **Start small, climb fast.** The first frame is a keyframe, the biggest there is: it goes at a
//!   modest bitrate, so it's small on a slow link, and while nothing has queued yet the target grows
//!   by half every quarter second (a LAN reaches the top in about a second and a half).
//! - **Cut to what got through.** On congestion the target drops to a little under what the client
//!   actually received over the last second (measured from the feedback), not by a fixed share: a
//!   cut neither overshoots the link nor stays above it, which is what made it oscillate.
//! - **Come back carefully.** After a cut it climbs quickly to just under where it last congested,
//!   then probes above that slowly.
//! - **Frame rate last.** Games need it: the frame rate holds (at the viewer's display rate, 60 at
//!   least) while the bitrate falls, then the picture shrinks; only at the lowest bitrate and the
//!   smallest picture does the frame rate go down.
//! - **Resolution follows.** When the bitrate is low for the picture's size, the picture is scaled
//!   down (¾, then ½) rather than smeared; it goes back up once the link has room, with hysteresis
//!   so it doesn't flap.
//!
//! That's `--prefer framerate` (the default). `--prefer quality` (text, detail) swaps the last two:
//! frames give way with the bitrate on each congestion, and the picture keeps its size until the
//! frame rate is at its least (roughly how the controller worked before frame rate came first).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub kbps: u32,
    pub fps: u32,
    /// The picture's scale: 1, ¾ or ½ of the source's size.
    pub scale: f32,
}

/// What gives way first on a slow link: frames or the picture (`--prefer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Prefer {
    /// The frame rate holds; the bitrate, then the picture's size give way (games, motion).
    #[default]
    Framerate,
    /// The picture keeps its size; frames give way with the bitrate (text, detail).
    Quality,
}

impl Prefer {
    pub fn parse(s: &str) -> Option<Prefer> {
        match s {
            "framerate" | "frame-rate" | "fps" => Some(Prefer::Framerate),
            "quality" => Some(Prefer::Quality),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Prefer::Framerate => "framerate",
            Prefer::Quality => "quality",
        }
    }
}

pub struct Adapt {
    prefer: Prefer,
    max: Target,
    min: Target,
    now: Target,
    /// The target last given out (tick says when `now` differs from it).
    given: Target,
    /// One-way delays (ms, with the clock offset) of the last few seconds, for the baseline.
    delays: VecDeque<(Instant, f64)>,
    /// Queuing (ms over the baseline) of the last half second.
    queuing: VecDeque<(Instant, f64)>,
    /// Bytes the client confirmed getting, and when: what the link delivers.
    delivered: VecDeque<(Instant, usize)>,
    backed_up: bool,
    /// The client lost a frame while nothing it got was coming back: nothing gets through.
    starved: bool,
    last_cut: Instant,
    last_step: Instant,
    /// Until the first congestion: climbing fast.
    starting: bool,
    /// The encoder skipped frames since the last tick: too few bits for the picture's size.
    encoder_short: bool,
    short_until: Option<Instant>,
    first_sent: Option<Instant>,
    last_sent: Option<Instant>,
    /// The target when it last congested: climbing back slows down below it.
    ceiling: Option<u32>,
    /// Since when the bitrate has been enough for a bigger picture (scaling back up waits a while).
    room_since: Option<Instant>,
}

const BASELINE_WINDOW: Duration = Duration::from_secs(10);
const RECENT: Duration = Duration::from_millis(500);
const DELIVERED_WINDOW: Duration = Duration::from_secs(1);
/// No feedback for this long: nothing is known about the link now.
const STALE: Duration = Duration::from_millis(500);
/// Queuing that means the link is full, and queuing low enough to try more.
const CONGESTED_MS: f64 = 40.0;
/// (The round trip includes the client's decoding, which wanders by 10–15 ms with the content.)
const CLEAR_MS: f64 = 20.0;
/// The first keyframe's bitrate: small on a slow link, quickly left behind on a fast one.
pub const START_KBPS: u32 = 800;
/// The scales, and the share of the top bitrate under which each is used.
const SCALES: [(f32, f64); 4] = [(1.0, 0.0), (0.75, 0.22), (0.5, 0.1), (1.0 / 3.0, 0.05)];
/// The smallest picture.
const SMALLEST: f32 = 1.0 / 3.0;

impl Adapt {
    pub fn new(max_kbps: u32, max_fps: u32) -> Self {
        let max = Target {
            kbps: max_kbps.max(300),
            fps: max_fps.max(5),
            scale: 1.0,
        };
        let long_ago = Instant::now() - Duration::from_secs(60);
        Self {
            prefer: Prefer::Framerate,
            max,
            min: Target {
                kbps: 250.min(max.kbps),
                fps: 5.min(max.fps),
                scale: SMALLEST,
            },
            now: Target {
                kbps: START_KBPS.min(max.kbps),
                fps: max.fps,
                scale: 1.0,
            },
            given: Target {
                kbps: START_KBPS.min(max.kbps),
                fps: max.fps,
                scale: 1.0,
            },
            delays: VecDeque::new(),
            queuing: VecDeque::new(),
            delivered: VecDeque::new(),
            backed_up: false,
            starved: false,
            encoder_short: false,
            short_until: None,
            first_sent: None,
            last_sent: None,
            last_cut: long_ago,
            last_step: long_ago,
            starting: true,
            ceiling: None,
            room_since: None,
        }
    }

    /// What gives way first (`Prefer::Framerate` unless set).
    pub fn prefer(mut self, p: Prefer) -> Self {
        self.prefer = p;
        self
    }

    pub fn target(&self) -> Target {
        self.now
    }

    /// A frame's one-way delay in ms (client receive time less capture time, clocks unsynced), and
    /// its size in bytes.
    pub fn feedback(&mut self, delay_ms: f64, bytes: usize, at: Instant) {
        self.delays.push_back((at, delay_ms));
        while self.delays.front().is_some_and(|(t, _)| at.duration_since(*t) > BASELINE_WINDOW) {
            self.delays.pop_front();
        }
        let base = self.delays.iter().map(|(_, d)| *d).fold(f64::INFINITY, f64::min);
        self.queuing.push_back((at, (delay_ms - base).max(0.0)));
        while self.queuing.front().is_some_and(|(t, _)| at.duration_since(*t) > RECENT) {
            self.queuing.pop_front();
        }
        self.delivered.push_back((at, bytes));
        while self.delivered.front().is_some_and(|(t, _)| at.duration_since(*t) > DELIVERED_WINDOW) {
            self.delivered.pop_front();
        }
    }

    /// The connection couldn't take what was sent (its buffer is full). Before any feedback it's the
    /// first keyframe still going out, not congestion: that's expected.
    pub fn backed_up(&mut self) {
        if !self.delays.is_empty() {
            self.backed_up = true;
        }
    }

    /// The client asked for a keyframe (it lost a frame). With feedback coming back that's the odd
    /// lost frame (random loss, which parity is for); with none for a while, nothing is getting
    /// through: as congested as it gets, even before the first feedback.
    pub fn frame_lost(&mut self, at: Instant) {
        if !self.feedback_lately(at) {
            self.starved = true;
        }
    }

    /// The encoder skipped more than a tenth of the frames given it since the last tick (a hardware
    /// encoder's rate control does, when a frame can't fit its share of the bitrate): the frame rate
    /// is kept by making the picture smaller.
    /// Starting, when it gave out nothing at all (`none_out`), it's the modest first bitrate that's
    /// short (a big fast picture at 800 kbit/s had every frame skipped, and with nothing sent no
    /// feedback comes to climb on): up it goes, as fast as starting climbs. Starting with some out,
    /// the feedback decides (on a slow link the bitrate must not climb blind).
    pub fn encoder_short(&mut self, _at: Instant, none_out: bool) {
        if self.starting && !(none_out && self.now.kbps >= self.max.kbps) {
            if none_out {
                self.now.kbps = ((self.now.kbps as f64 * 1.5) as u32).min(self.max.kbps);
            }
        } else {
            // Nothing out even at the top bitrate (an encoder that can't do this size at this rate:
            // H.264 at 4K and 240 fps): smaller.
            self.starting = false;
            self.encoder_short = true;
        }
    }

    /// A frame went out. Frames going out for a second with no feedback for a while: nothing gets
    /// through (or nothing the client can use: no feedback comes until a frame decodes).
    pub fn sent(&mut self, at: Instant) {
        self.first_sent.get_or_insert(at);
        self.last_sent = Some(at);
    }

    fn feedback_lately(&self, at: Instant) -> bool {
        self.delivered.back().is_some_and(|(t, _)| at.duration_since(*t) < STALE)
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

    /// What the client confirmed getting over the last second, kbit/s (0 until there's a second of it).
    pub fn delivered_kbps(&self, at: Instant) -> u32 {
        let Some((first, _)) = self.delivered.front() else { return 0 };
        let span = at.duration_since(*first).as_secs_f64().max(0.25);
        let bytes: usize = self.delivered.iter().map(|(_, b)| b).sum();
        (bytes as f64 * 8.0 / 1000.0 / span) as u32
    }

    /// Called a few times a second: a new target when it should change.
    pub fn tick(&mut self, at: Instant) -> Option<Target> {
        let q = self.queuing_ms();
        let sending = self.last_sent.is_some_and(|l| at.duration_since(l) < Duration::from_millis(250))
            && self.first_sent.is_some_and(|f| at.duration_since(f) > Duration::from_secs(1));
        if sending && !self.feedback_lately(at) {
            self.starved = true;
        }
        if (self.backed_up || self.starved || q > CONGESTED_MS) && at.duration_since(self.last_cut) > Duration::from_millis(300) {
            // Congested: to a little under what got through (or by 30% when that's not known yet).
            let got = self.delivered_kbps(at);
            let to = if got > 0 {
                (got as f64 * 0.85) as u32
            } else {
                (self.now.kbps as f64 * 0.7) as u32
            };
            // The ceiling is where this bout of congestion began (a second cut in a row isn't one).
            if at.duration_since(self.last_cut) > Duration::from_secs(2) {
                self.ceiling = Some(self.now.kbps);
            }
            // Frame rate first: it goes down only once there's nothing else left to give. Quality:
            // it goes down with the bitrate, each time.
            if self.prefer == Prefer::Quality || self.now.kbps <= self.min.kbps && self.now.scale <= SMALLEST {
                self.now.fps = ((self.now.fps as f64 * 0.75) as u32).max(self.min.fps);
            }
            self.now.kbps = to.min((self.now.kbps as f64 * 0.9) as u32).max(self.min.kbps);
            self.last_cut = at;
            self.starting = false;
            self.room_since = None;
        } else if q < CLEAR_MS
            && !self.backed_up
            && !self.starved
            // Only on what came back: with no feedback, no queuing shows, which says nothing.
            && self.feedback_lately(at)
            && at.duration_since(self.last_cut) > Duration::from_secs(1)
            && at.duration_since(self.last_step) > Duration::from_millis(250)
        {
            // Clear: up. Fast while starting; quickly back to under the last ceiling; slowly above it.
            let k = self.now.kbps as f64;
            let next = if self.starting {
                k * 1.5
            } else {
                match self.ceiling {
                    Some(c) if k < c as f64 * 0.9 => (k * 1.15).min(c as f64 * 0.9),
                    _ => k * 1.03 + 20.0,
                }
            };
            self.now.kbps = (next as u32).max(self.now.kbps + 1).min(self.max.kbps);
            // At the top: started.
            if self.now.kbps >= self.max.kbps {
                self.starting = false;
            }
            let held = self.prefer == Prefer::Quality && self.short_until.is_some_and(|u| at < u);
            if !held {
                self.now.fps = ((self.now.fps as f64 * 1.25) as u32 + 1).min(self.max.fps);
            }
            self.last_step = at;
        }
        self.backed_up = false;
        self.starved = false;
        // Starting, the picture stays whole (its first keyframe is small by its bitrate already).
        if !self.starting {
            let scale = self.scale_for(at);
            // Quality: the picture shrinks only once the frame rate is at its least.
            let keep = self.prefer == Prefer::Quality && scale < self.now.scale && self.now.fps > self.min.fps;
            if !keep {
                self.now.scale = scale;
            }
        }
        // (A second between: each new size is a new encoder, which takes a moment to give out frames.)
        let shrunk_lately = self.short_until.is_some_and(|u| u > at + Duration::from_secs(4));
        let short = std::mem::take(&mut self.encoder_short);
        if short && self.prefer == Prefer::Quality && self.now.fps > self.min.fps {
            // Quality: fewer frames for the encoder before a smaller picture (held a while: climbing
            // straight back would only be short again).
            self.now.fps = ((self.now.fps as f64 * 0.75) as u32).max(self.min.fps);
            self.short_until = Some(at + Duration::from_secs(5));
        } else if short && !shrunk_lately {
            // A size smaller than both what the bitrate picks and what it is now.
            let smaller = SCALES.iter().map(|(s, _)| *s).filter(|s| *s < self.now.scale).fold(0.0f32, f32::max);
            if smaller > 0.0 {
                self.now.scale = smaller;
            }
            self.room_since = None;
            self.short_until = Some(at + Duration::from_secs(5));
        }
        if self.now == self.given {
            return None;
        }
        self.given = self.now;
        Some(self.now)
    }

    /// The picture's scale for the bitrate now: down at once when it's low, back up only after
    /// the bitrate has had room for the bigger picture for two seconds.
    fn scale_for(&mut self, at: Instant) -> f32 {
        // Not back up for a while after the encoder ran short at a bigger size.
        let held = self.short_until.is_some_and(|u| at < u);
        let share = self.now.kbps as f64 / self.max.kbps as f64;
        let fits = SCALES.iter().filter(|(_, under)| share < *under).map(|(s, _)| *s).fold(1.0f32, f32::min);
        if fits <= self.now.scale || held {
            self.room_since = None;
            return fits.min(self.now.scale);
        }
        // Room for a bigger picture: a third above the threshold, held for two seconds.
        let bigger = SCALES.iter().map(|(s, _)| *s).filter(|s| *s > self.now.scale).fold(1.0f32, f32::min);
        let need = SCALES.iter().find(|(s, _)| *s == self.now.scale).map_or(0.0, |(_, under)| under * 1.33);
        if share < need {
            self.room_since = None;
            return self.now.scale;
        }
        let since = *self.room_since.get_or_insert(at);
        if at.duration_since(since) >= Duration::from_secs(2) {
            self.room_since = None;
            bigger
        } else {
            self.now.scale
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `ms` of one-way delay (with a clock offset) for `secs`, ticking as the server does, over
    /// a link that carries `cap` kbit/s; the lowest target it reached.
    fn run_on(a: &mut Adapt, t: &mut Instant, secs: f64, cap: u32, delay: impl Fn(f64) -> f64) -> Target {
        let steps = (secs * 60.0) as usize;
        let mut low = a.target();
        for i in 0..steps {
            *t += Duration::from_millis(16);
            // Each frame its share of what gets through: the target, up to the link's capacity.
            let bytes = (a.target().kbps.min(cap) as usize * 1000 / 8) / 60;
            a.feedback(1_000_000.0 + delay(i as f64 / 60.0), bytes, *t);
            if i % 15 == 0 {
                a.tick(*t);
                if a.target().kbps < low.kbps {
                    low = a.target();
                }
            }
        }
        low
    }

    /// The same over a link with room for everything.
    fn run(a: &mut Adapt, t: &mut Instant, secs: f64, delay: impl Fn(f64) -> f64) -> Target {
        run_on(a, t, secs, u32::MAX, delay)
    }

    #[test]
    fn no_feedback_no_climbing_and_lost_frames_with_none_cut() {
        // A link so full nothing comes back: the first keyframe never arrives. Climbing on silence
        // sent 8 Mbit/s into a 1.5 Mbit/s link and stayed there (tests/netem).
        let mut a = Adapt::new(8000, 60);
        let mut t = Instant::now();
        for _ in 0..40 {
            t += Duration::from_millis(250);
            a.tick(t);
        }
        assert_eq!(a.target().kbps, START_KBPS, "no feedback, no climbing");
        // The client keeps losing frames and asking for keyframes: each is a cut.
        for _ in 0..8 {
            t += Duration::from_millis(400);
            a.frame_lost(t);
            a.tick(t);
        }
        assert_eq!(a.target().kbps, 250, "down to the floor");
        // With feedback coming back, a lost frame is random loss (parity's job), not congestion.
        let mut a = Adapt::new(8000, 60);
        let mut t = Instant::now();
        run(&mut a, &mut t, 10.0, |_| 20.0);
        let before = a.target().kbps;
        a.frame_lost(t);
        a.tick(t);
        assert_eq!(a.target().kbps, before);
    }

    #[test]
    fn backs_off_when_delay_builds_and_climbs_back_when_clear() {
        let mut a = Adapt::new(8000, 60);
        let mut t = Instant::now();
        run(&mut a, &mut t, 20.0, |_| 20.0);
        assert_eq!(
            a.target(),
            Target {
                kbps: 8000,
                fps: 60,
                scale: 1.0
            },
            "a clear link climbs to the top and stays"
        );
        // The link (2 Mbit/s) fills: queuing grows by 30 ms a second.
        run_on(&mut a, &mut t, 3.0, 2000, |s| 20.0 + s * 30.0);
        assert!(a.target().kbps < 3000, "it backed off: {:?}", a.target());
        assert_eq!(a.target().fps, 60, "the frame rate held");
        // Clear again (the queue drains for a moment first): it climbs back, slowly.
        let low = run(&mut a, &mut t, 3.0, |_| 20.0);
        let mid = a.target();
        assert!(mid.kbps > low.kbps && mid.kbps < 8000, "climbing: {mid:?}");
        run(&mut a, &mut t, 30.0, |_| 20.0);
        assert_eq!(
            a.target(),
            Target {
                kbps: 8000,
                fps: 60,
                scale: 1.0
            }
        );
    }

    #[test]
    fn a_backed_up_connection_is_congestion() {
        let mut a = Adapt::new(8000, 60);
        let t = Instant::now() + Duration::from_secs(1);
        // The first keyframe going out isn't congestion (no cut: it climbs); after feedback, a backlog is.
        a.backed_up();
        assert!(a.tick(t).is_none_or(|n| n.kbps > START_KBPS));
        let t = t + Duration::from_secs(1);
        a.feedback(30.0, 1000, t);
        a.backed_up();
        // Cut to under what got through (a trickle here: the floor), the frame rate kept.
        assert_eq!(a.tick(t).map(|t| (t.kbps, t.fps)), Some((250, 60)));
    }

    #[test]
    fn the_frame_rate_goes_last() {
        // Congested again and again: bitrate and size give way first; the full frame rate until both are at
        // their least.
        let mut a = Adapt::new(8000, 120);
        let mut t = Instant::now();
        a.feedback(30.0, 1000, t);
        let mut seen = vec![];
        for _ in 0..40 {
            t += Duration::from_millis(400);
            a.backed_up();
            a.tick(t);
            seen.push(a.target());
        }
        for s in &seen {
            assert!(s.fps == 120 || (s.kbps == 250 && s.scale == SMALLEST), "{s:?}");
        }
    }

    #[test]
    fn quality_gives_up_frames_before_the_picture() {
        // Congested again and again: frames go with the bitrate, and the picture keeps its size until
        // the frame rate is at its least.
        let mut a = Adapt::new(8000, 120).prefer(Prefer::Quality);
        let mut t = Instant::now();
        a.feedback(30.0, 1000, t);
        let mut seen = vec![];
        for _ in 0..40 {
            t += Duration::from_millis(400);
            a.backed_up();
            a.tick(t);
            seen.push(a.target());
        }
        for s in &seen {
            assert!(s.scale == 1.0 || s.fps == 5, "the picture shrank before the frames went: {s:?}");
        }
        assert!(seen.iter().any(|s| s.fps < 60 && s.scale == 1.0), "frames went first: {seen:?}");
        assert_eq!(
            a.target(),
            Target {
                kbps: 250,
                fps: 5,
                scale: SMALLEST
            },
            "and at the end, both at their least"
        );
        // With room again, all of it comes back.
        run(&mut a, &mut t, 60.0, |_| 20.0);
        assert_eq!(
            a.target(),
            Target {
                kbps: 8000,
                fps: 120,
                scale: 1.0
            }
        );
        // An encoder short of time: fewer frames, the same picture.
        a.encoder_short(t, false);
        a.tick(t);
        assert_eq!((a.target().fps, a.target().scale), (90, 1.0));
        // Frame rate first stays the default.
        assert_eq!(Adapt::new(8000, 60).prefer, Prefer::Framerate);
        assert_eq!(Prefer::parse("quality"), Some(Prefer::Quality));
        assert_eq!(Prefer::parse("framerate"), Some(Prefer::Framerate));
        assert_eq!(Prefer::parse("speed"), None);
    }

    #[test]
    fn an_encoder_skipping_frames_makes_the_picture_smaller_not_the_frame_rate() {
        // Starting, the bitrate climbs instead, feedback or none (with every frame skipped, none comes).
        let mut a = Adapt::new(8000, 240);
        let mut t = Instant::now();
        a.encoder_short(t, true);
        a.tick(t);
        assert_eq!((a.target().scale, a.target().kbps), (1.0, START_KBPS * 3 / 2));
        // Some frames out: the feedback decides, not the encoder.
        a.encoder_short(t, false);
        assert_eq!(a.target().kbps, START_KBPS * 3 / 2);
        // Nothing out even at the top: smaller.
        let mut b = Adapt::new(8000, 240);
        for _ in 0..10 {
            b.encoder_short(t, true);
            b.tick(t);
        }
        // (Held a second before it shrinks again.)
        assert_eq!((b.target().kbps, b.target().scale, b.target().fps), (8000, 0.75, 240));
        a.feedback(1_000_020.0, 1000, t);
        // At the top, the picture gives way.
        run(&mut a, &mut t, 5.0, |_| 20.0);
        assert_eq!(a.target().scale, 1.0);
        a.encoder_short(t, false);
        a.tick(t);
        assert_eq!((a.target().scale, a.target().fps), (0.75, 240));
        // Not again at once (the new encoder needs a moment); a second later, yes.
        a.encoder_short(t, false);
        a.tick(t);
        assert_eq!(a.target().scale, 0.75);
        run(&mut a, &mut t, 1.1, |_| 20.0);
        a.encoder_short(t, false);
        a.tick(t);
        assert_eq!(a.target().scale, 0.5);
        // Held there a while, then back up with room.
        run(&mut a, &mut t, 2.0, |_| 20.0);
        assert_eq!(a.target().scale, 0.5);
        run(&mut a, &mut t, 20.0, |_| 20.0);
        assert_eq!(a.target().scale, 1.0);
    }

    #[test]
    fn frames_going_out_with_no_feedback_is_starved() {
        // A link too slow for even the first keyframe: frames go, nothing decodes, nothing comes back.
        let mut a = Adapt::new(8000, 60);
        let t0 = Instant::now();
        let mut t = t0;
        for _ in 0..120 {
            t += Duration::from_millis(16);
            a.sent(t);
            if (t - t0).as_millis() % 250 < 16 {
                a.tick(t);
            }
        }
        assert!(a.target().kbps < START_KBPS, "{:?}", a.target());
    }

    #[test]
    fn never_below_the_floor() {
        let mut a = Adapt::new(8000, 60);
        let mut t = Instant::now();
        a.feedback(30.0, 1000, t);
        for _ in 0..100 {
            t += Duration::from_millis(400);
            a.backed_up();
            a.tick(t);
        }
        assert_eq!(
            a.target(),
            Target {
                kbps: 250,
                fps: 5,
                scale: SMALLEST
            }
        );
    }

    #[test]
    fn starts_small_and_reaches_the_top_fast_on_a_clear_link() {
        let mut a = Adapt::new(8000, 60);
        assert_eq!(a.target().kbps, START_KBPS);
        let mut t = Instant::now();
        // Clear: within two seconds (the first second waits, as after a cut) it's at the top.
        run(&mut a, &mut t, 3.0, |_| 20.0);
        assert_eq!(a.target().kbps, 8000);
    }

    #[test]
    fn a_cut_goes_to_under_what_got_through_and_stays_near_it() {
        let mut a = Adapt::new(8000, 60);
        let mut t = Instant::now();
        run(&mut a, &mut t, 5.0, |_| 20.0);
        // A 1.5 Mbit/s link: frames of 3125 bytes at 60 a second; queuing builds.
        for i in 0..120 {
            t += Duration::from_millis(16);
            a.feedback(1_000_000.0 + 20.0 + i as f64 * 2.0, 3125, t);
            if i % 15 == 0 {
                a.tick(t);
            }
        }
        let cut = a.target().kbps;
        assert!((800..=1500).contains(&cut), "cut to under the 1.5 Mbit/s that got through: {cut}");
        // Clear again: it comes back toward that, not to 8 Mbit/s at once.
        run(&mut a, &mut t, 4.0, |_| 20.0);
        assert!(a.target().kbps < 3000, "{:?}", a.target());
    }

    #[test]
    fn the_picture_scales_down_on_a_slow_link_and_back_up_with_room() {
        let mut a = Adapt::new(8000, 60);
        let mut t = Instant::now();
        a.feedback(30.0, 1000, t);
        for _ in 0..30 {
            t += Duration::from_millis(400);
            a.backed_up();
            a.tick(t);
        }
        assert_eq!(a.target().scale, SMALLEST, "250 kbit/s of 8000: a third of the size");
        run(&mut a, &mut t, 60.0, |_| 20.0);
        assert_eq!(a.target().scale, 1.0, "room again: full size");
    }
}
