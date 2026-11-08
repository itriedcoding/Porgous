//! Simple per-IP fixed-window rate limiting.

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<String, (i64, u32)>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true when the request is allowed.
    pub fn check(&self, key: &str, limit: u32) -> bool {
        if limit == 0 {
            return true;
        }
        let minute = crate::db::now_ms() / 60_000;
        let mut buckets = self.buckets.lock().unwrap();
        if buckets.len() > 20_000 {
            buckets.retain(|_, (window, _)| *window >= minute);
        }
        let entry = buckets.entry(key.to_string()).or_insert((minute, 0));
        if entry.0 != minute {
            *entry = (minute, 1);
            true
        } else if entry.1 >= limit {
            false
        } else {
            entry.1 += 1;
            true
        }
    }

    /// Seconds until the current window resets.
    pub fn retry_after(&self) -> u64 {
        60 - (crate::db::now_ms() as u64 / 1000) % 60
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_limits() {
        let limiter = RateLimiter::new();
        for _ in 0..3 {
            assert!(limiter.check("ip", 3));
        }
        assert!(!limiter.check("ip", 3));
        assert!(limiter.check("other", 3));
    }
}
