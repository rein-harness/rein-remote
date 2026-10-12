//! Windows: what the standard library doesn't give.

use std::time::Duration;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// This process's CPU time so far (user and kernel).
pub fn cpu_time() -> Duration {
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: the current process's pseudo-handle and four out-parameters.
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }.is_err() {
        return Duration::ZERO;
    }
    let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) * 100;
    Duration::from_nanos(t(k) + t(u))
}
