//! Idle unloading with an injectable clock.

use std::time::{Duration, Instant};

/// Monotonic time source (offset from an arbitrary epoch).
pub trait Clock {
    fn now(&self) -> Duration;
}

pub struct SystemClock(Instant);

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock(Instant::now())
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Tracks the last use; `should_reap` is true once `timeout` has passed without `touch`.
pub struct IdleReaper<C: Clock = SystemClock> {
    clock: C,
    timeout: Duration,
    last: Duration,
}

impl IdleReaper<SystemClock> {
    pub fn new(timeout: Duration) -> Self {
        Self::with_clock(SystemClock::default(), timeout)
    }
}

impl<C: Clock> IdleReaper<C> {
    pub fn with_clock(clock: C, timeout: Duration) -> Self {
        let last = clock.now();
        IdleReaper { clock, timeout, last }
    }
    pub fn touch(&mut self) {
        self.last = self.clock.now();
    }
    pub fn idle_for(&self) -> Duration {
        self.clock.now().saturating_sub(self.last)
    }
    pub fn should_reap(&self) -> bool {
        self.idle_for() >= self.timeout
    }
}

/// A lazily created resource (e.g. a `LlamaServer`) that is dropped after idling.
/// Call `get_or_init` per request and `tick` periodically from a timer.
pub struct IdleSlot<T, C: Clock = SystemClock> {
    value: Option<T>,
    reaper: IdleReaper<C>,
}

impl<T> IdleSlot<T, SystemClock> {
    pub fn new(timeout: Duration) -> Self {
        IdleSlot { value: None, reaper: IdleReaper::new(timeout) }
    }
}

impl<T, C: Clock> IdleSlot<T, C> {
    pub fn with_clock(clock: C, timeout: Duration) -> Self {
        IdleSlot { value: None, reaper: IdleReaper::with_clock(clock, timeout) }
    }

    pub fn get_or_init<E>(&mut self, init: impl FnOnce() -> Result<T, E>) -> Result<&mut T, E> {
        if self.value.is_none() {
            self.value = Some(init()?);
        }
        self.reaper.touch();
        Ok(self.value.as_mut().expect("just set"))
    }

    /// Drops the value if idle. Returns true if it was dropped.
    pub fn tick(&mut self) -> bool {
        if self.value.is_some() && self.reaper.should_reap() {
            self.value = None;
            return true;
        }
        false
    }

    pub fn is_loaded(&self) -> bool {
        self.value.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct FakeClock(Rc<Cell<u64>>);
    impl FakeClock {
        fn advance(&self, s: u64) {
            self.0.set(self.0.get() + s);
        }
    }
    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            Duration::from_secs(self.0.get())
        }
    }

    #[test]
    fn reaper_timing() {
        let c = FakeClock::default();
        let mut r = IdleReaper::with_clock(c.clone(), Duration::from_secs(120));
        c.advance(119);
        assert!(!r.should_reap());
        r.touch();
        c.advance(119);
        assert!(!r.should_reap());
        c.advance(1);
        assert!(r.should_reap());
        assert_eq!(r.idle_for(), Duration::from_secs(120));
    }

    #[test]
    fn slot_lifecycle() {
        let c = FakeClock::default();
        let mut s: IdleSlot<u32, _> = IdleSlot::with_clock(c.clone(), Duration::from_secs(60));
        let inits = Cell::new(0);
        let get = |s: &mut IdleSlot<u32, FakeClock>| {
            *s.get_or_init::<()>(|| {
                inits.set(inits.get() + 1);
                Ok(7)
            })
            .unwrap()
        };
        assert!(!s.tick());
        assert_eq!(get(&mut s), 7);
        c.advance(59);
        assert!(!s.tick());
        assert_eq!(get(&mut s), 7); // touch resets
        c.advance(59);
        assert!(!s.tick() && s.is_loaded());
        c.advance(1);
        assert!(s.tick() && !s.is_loaded());
        assert_eq!(get(&mut s), 7);
        assert_eq!(inits.get(), 2);
    }

    #[test]
    fn init_error_leaves_empty() {
        let mut s: IdleSlot<u32> = IdleSlot::new(Duration::from_secs(1));
        assert!(s.get_or_init(|| Err::<u32, _>("boom")).is_err());
        assert!(!s.is_loaded());
    }
}
