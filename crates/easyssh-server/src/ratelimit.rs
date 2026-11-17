//! Fixed-window rate limiter keyed by an arbitrary string.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    window: Duration,
    entries: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Count one hit for `key`. `Err(seconds)` says when to retry.
    pub fn check(&self, key: &str, limit: u32) -> Result<(), u64> {
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if entries.len() > 10_000 {
            let window = self.window;
            entries.retain(|_, (start, _)| now.duration_since(*start) < window);
        }
        let entry = entries.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= limit {
            let left = self.window.saturating_sub(now.duration_since(entry.0));
            return Err(left.as_secs().max(1));
        }
        entry.1 += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_limit_and_keys_are_independent() {
        let limiter = RateLimiter::new(Duration::from_secs(60));
        for _ in 0..3 {
            limiter.check("a", 3).unwrap();
        }
        assert!(limiter.check("a", 3).is_err());
        limiter.check("b", 3).unwrap();
    }

    #[test]
    fn window_resets() {
        let limiter = RateLimiter::new(Duration::from_millis(20));
        limiter.check("a", 1).unwrap();
        assert!(limiter.check("a", 1).is_err());
        std::thread::sleep(Duration::from_millis(30));
        limiter.check("a", 1).unwrap();
    }
}
