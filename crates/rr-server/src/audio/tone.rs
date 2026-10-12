//! A test tone for the test pattern: silence, and a 1 kHz beep of 100 ms starting at the very
//! microsecond the pattern's flash was captured (`capture::FLASH_ONSET_US`). Sound and picture are in
//! sync here by construction, so whatever offset a client measures between the beep it plays and the
//! flash it shows is the pipeline's.

use super::{now_us, Block, Capture, Sink, FRAME, RATE};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const BEEP_HZ: f32 = 1000.0;
pub const BEEP_US: u64 = 100_000;

pub struct Tone {
    stop: Arc<AtomicBool>,
}

impl Capture for Tone {}

impl Drop for Tone {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The tone's sample at `t_us`, for a flash that started at `onset_us`.
pub fn sample(t_us: u64, onset_us: u64) -> f32 {
    if onset_us == 0 || t_us < onset_us || t_us - onset_us >= BEEP_US {
        return 0.0;
    }
    0.5 * ((t_us - onset_us) as f32 / 1e6 * BEEP_HZ * 2.0 * std::f32::consts::PI).sin()
}

impl Tone {
    pub fn start(sink: Sink) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        std::thread::spawn(move || {
            let (base_us, base) = (now_us(), Instant::now());
            for k in 0u64.. {
                // Each 10 ms block a moment after it ends, as a sound card would hand it over (and
                // after any flash in it was drawn).
                let due = base + Duration::from_micros((k + 1) * 10_000 + 2_000);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
                if done.load(Ordering::Relaxed) {
                    return;
                }
                let onset = crate::capture::FLASH_ONSET_US.load(Ordering::Relaxed);
                let at_us = base_us + k * 10_000;
                let samples = (0..FRAME)
                    .flat_map(|i| {
                        let s = sample(at_us + (i as u64 * 1_000_000 / RATE as u64), onset);
                        [s, s]
                    })
                    .collect();
                if sink.send(Block { at_us, samples }).is_err() {
                    return;
                }
            }
        });
        Self { stop }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beeps_from_the_flash_on() {
        assert_eq!(sample(5_000_000, 0), 0.0);
        assert_eq!(sample(4_999_999, 5_000_000), 0.0);
        assert_eq!(sample(5_000_000, 5_000_000), 0.0); // starts at a zero crossing
        assert!((sample(5_000_250, 5_000_000) - 0.5).abs() < 1e-3); // a quarter period in
        assert_eq!(sample(5_100_000, 5_000_000), 0.0);
    }
}
