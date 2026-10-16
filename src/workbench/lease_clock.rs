//! Authorization age uses a monotonic clock that includes system suspension.
//! Tokio/std Instant timers are wake-up hints, never the authorization oracle.
use std::time::Duration;

#[derive(Clone, Default)]
pub(crate) enum Clock {
    #[default]
    System,
    #[cfg(test)]
    Manual(std::sync::Arc<std::sync::Mutex<Option<Duration>>>),
}
impl Clock {
    fn now(&self) -> Option<Duration> {
        match self {
            Self::System => continuous_now(),
            #[cfg(test)]
            Self::Manual(value) => *value.lock().unwrap(),
        }
    }
    #[cfg(test)]
    pub(crate) fn manual(value: Duration) -> Self {
        Self::Manual(std::sync::Arc::new(std::sync::Mutex::new(Some(value))))
    }
    #[cfg(test)]
    pub(crate) fn set(&self, value: Option<Duration>) {
        let Self::Manual(clock) = self else {
            panic!("not a test clock")
        };
        *clock.lock().unwrap() = value;
    }
}

pub(crate) struct Lease {
    clock: Clock,
    renewed_at: Duration,
    deadline: Duration,
}
impl Lease {
    pub(crate) fn new(clock: Clock, duration: Duration) -> Option<Self> {
        let now = clock.now()?;
        Some(Self {
            clock,
            renewed_at: now,
            deadline: now.checked_add(duration)?,
        })
    }
    fn remaining_at(&self, now: Duration) -> Option<Duration> {
        // A failed or backwards clock is not evidence that authority is current.
        now.checked_sub(self.renewed_at)?;
        self.deadline
            .checked_sub(now)
            .filter(|left| !left.is_zero())
    }
    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.remaining_at(self.clock.now()?)
    }
    pub(crate) fn renew(&mut self, duration: Duration) -> bool {
        let Some(now) = self.clock.now() else {
            return false;
        };
        if self.remaining_at(now).is_none() {
            return false;
        }
        let Some(deadline) = now.checked_add(duration) else {
            return false;
        };
        self.renewed_at = now;
        self.deadline = deadline;
        true
    }
}

#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn continuous_now() -> Option<Duration> {
    // Apple CLOCK_MONOTONIC_RAW is mach_continuous_time in nanoseconds.
    // Linux CLOCK_MONOTONIC_RAW excludes suspend; CLOCK_BOOTTIME includes it.
    #[cfg(target_vendor = "apple")]
    let id = libc::CLOCK_MONOTONIC_RAW;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let id = libc::CLOCK_BOOTTIME;
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: value is initialized, correctly aligned, and writable for the call.
    if unsafe { libc::clock_gettime(id, &mut value) } != 0 {
        return None;
    }
    timespec_duration(value.tv_sec, value.tv_nsec)
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn continuous_now() -> Option<Duration> {
    // No silent uptime/wall-clock fallback on an unverified platform.
    None
}

#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn timespec_duration(seconds: libc::time_t, nanoseconds: libc::c_long) -> Option<Duration> {
    if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
        return None;
    }
    Some(Duration::new(
        u64::try_from(seconds).ok()?,
        u32::try_from(nanoseconds).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suspended_time_and_clock_failure_expire_without_advancing_awake_clock() {
        let awake = tokio::time::Instant::now();
        let clock = Clock::manual(Duration::from_secs(100));
        let mut lease = Lease::new(clock.clone(), Duration::from_secs(30)).unwrap();
        clock.set(Some(Duration::from_secs(131)));
        assert!(awake.elapsed() < Duration::from_secs(1));
        assert!(lease.remaining().is_none());
        assert!(!lease.renew(Duration::from_secs(30)));
        for value in [None, Some(Duration::from_secs(99))] {
            clock.set(value);
            assert!(lease.remaining().is_none());
            assert!(!lease.renew(Duration::from_secs(30)));
        }
        clock.set(Some(Duration::MAX));
        assert!(Lease::new(clock, Duration::from_secs(30)).is_none());
    }
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    #[test]
    fn platform_clock_works_and_invalid_timespec_is_rejected() {
        let before = continuous_now().expect("platform continuous clock");
        assert!(continuous_now().unwrap() >= before);
        assert!(timespec_duration(-1, 0).is_none());
        assert!(timespec_duration(1, -1).is_none());
        assert!(timespec_duration(1, 1_000_000_000).is_none());
        assert_eq!(timespec_duration(1, 2), Some(Duration::new(1, 2)));
    }
}
