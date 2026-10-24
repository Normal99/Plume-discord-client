//! Discord tells us our budget in response headers:
//! `X-RateLimit-Remaining` (requests left) and `X-RateLimit-Reset-After`
//! (seconds until it refills). We wait *before* sending when a bucket is
//! empty, instead of sending and getting a 429.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::header::HeaderMap;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    remaining: u32,
    reset_at: Instant,
}

#[derive(Debug, Default)]
pub struct RateLimiter {
    // A plain Mutex is fine: it's never held across an `.await`.
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    buckets: HashMap<String, Bucket>,
    /// Set when Discord says *every* request must wait.
    global_until: Option<Instant>,
}

impl RateLimiter {
    /// Waits until a request in this bucket is allowed, then reserves it.
    pub async fn acquire(&self, key: &str) {
        loop {
            match self.try_acquire(key, Instant::now()) {
                None => return,
                Some(wait) => tokio::time::sleep(wait).await,
            }
        }
    }

    /// Reserves a request if allowed now, else says how long to wait.
    fn try_acquire(&self, key: &str, now: Instant) -> Option<Duration> {
        let mut state = self.state.lock().expect("rate limiter lock poisoned");

        if let Some(until) = state.global_until {
            if until > now {
                return Some(until - now);
            }
            state.global_until = None;
        }

        let Some(bucket) = state.buckets.get_mut(key) else {
            return None; // Never seen this route: just try it.
        };
        if bucket.reset_at <= now {
            state.buckets.remove(key); // Refilled; the response will tell us the new budget.
            return None;
        }
        if bucket.remaining == 0 {
            return Some(bucket.reset_at - now);
        }
        bucket.remaining -= 1;
        None
    }

    /// Records the budget from a response's headers.
    pub fn update(&self, key: &str, headers: &HeaderMap) {
        let number = |name: &str| headers.get(name)?.to_str().ok()?.parse::<f64>().ok();
        let (Some(remaining), Some(reset_after)) = (number("x-ratelimit-remaining"), number("x-ratelimit-reset-after"))
        else {
            return;
        };
        let bucket = Bucket {
            remaining: remaining as u32,
            reset_at: Instant::now() + Duration::from_secs_f64(reset_after.max(0.0)),
        };
        self.state
            .lock()
            .expect("rate limiter lock poisoned")
            .buckets
            .insert(key.to_owned(), bucket);
    }

    /// After a 429: block one bucket, or everything if `global`.
    pub fn penalize(&self, key: &str, retry_after: Duration, global: bool) {
        let until = Instant::now() + retry_after;
        let mut state = self.state.lock().expect("rate limiter lock poisoned");
        if global {
            state.global_until = Some(until);
        } else {
            state.buckets.insert(
                key.to_owned(),
                Bucket {
                    remaining: 0,
                    reset_at: until,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(remaining: &str, reset_after: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining", HeaderValue::from_str(remaining).unwrap());
        headers.insert("x-ratelimit-reset-after", HeaderValue::from_str(reset_after).unwrap());
        headers
    }

    #[test]
    fn unknown_buckets_are_allowed() {
        let limiter = RateLimiter::default();
        assert_eq!(limiter.try_acquire("a", Instant::now()), None);
    }

    #[test]
    fn empty_bucket_waits_until_reset() {
        let limiter = RateLimiter::default();
        limiter.update("a", &headers("1", "2.5"));
        let now = Instant::now();

        assert_eq!(limiter.try_acquire("a", now), None, "one request left");
        let wait = limiter.try_acquire("a", now).expect("bucket is empty now");
        assert!(wait > Duration::from_secs(2) && wait <= Duration::from_millis(2500));

        // Other buckets aren't affected.
        assert_eq!(limiter.try_acquire("b", now), None);
        // After the reset it's allowed again.
        assert_eq!(limiter.try_acquire("a", now + Duration::from_secs(3)), None);
    }

    #[test]
    fn global_limit_blocks_every_bucket() {
        let limiter = RateLimiter::default();
        limiter.penalize("a", Duration::from_secs(1), true);
        assert!(limiter.try_acquire("anything", Instant::now()).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_sleeps_through_the_wait() {
        let limiter = RateLimiter::default();
        limiter.penalize("a", Duration::from_secs(5), false);
        let start = Instant::now();
        limiter.acquire("a").await; // with a paused clock this "sleeps" instantly
        assert!(start.elapsed() >= Duration::from_secs(5));
    }
}
