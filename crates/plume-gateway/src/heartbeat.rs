use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::Instant;

/// Heartbeat timing for one connection. If Discord doesn't ACK a beat
/// before the next one is due, the connection is a "zombie" (open but dead)
/// and we reconnect.
pub struct Heartbeat {
    interval: Duration,
    next: Instant,
    awaiting_ack: bool,
    sent_at: Option<Instant>,
    /// How long the last ACK took.
    pub latency: Option<Duration>,
}

pub enum Beat {
    /// Send a heartbeat now.
    Send,
    /// The last one was never acknowledged.
    Zombie,
}

impl Heartbeat {
    /// Not started yet: waits until HELLO says how often to beat.
    pub fn idle() -> Self {
        Heartbeat {
            interval: Duration::from_secs(41),
            next: Instant::now() + Duration::from_secs(60 * 60 * 24),
            awaiting_ack: false,
            sent_at: None,
            latency: None,
        }
    }

    /// Starts beating every `interval`. The first beat is at a random point
    /// in the first interval (as Discord asks) so clients that reconnect
    /// together don't beat together.
    pub fn start(interval: Duration) -> Self {
        let interval = interval.max(Duration::from_secs(1));
        Heartbeat {
            interval,
            next: Instant::now() + interval.mul_f64(jitter()),
            ..Heartbeat::idle()
        }
    }

    pub fn next(&self) -> Instant {
        self.next
    }

    /// Called when `next()` is reached.
    pub fn due(&mut self) -> Beat {
        if self.awaiting_ack {
            return Beat::Zombie;
        }
        self.sent();
        Beat::Send
    }

    /// Records a beat sent now (also used when Discord asks for one early).
    pub fn sent(&mut self) {
        let now = Instant::now();
        self.awaiting_ack = true;
        self.sent_at = Some(now);
        self.next = now + self.interval;
    }

    pub fn acked(&mut self) {
        self.awaiting_ack = false;
        self.latency = self.sent_at.map(|at| at.elapsed());
    }
}

/// A number in [0, 1). Good enough for spreading out timers, without
/// pulling in a random number crate.
pub fn jitter() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    // Scramble the low bits a little so consecutive calls differ.
    let mixed = nanos.wrapping_mul(2_654_435_761) % 1_000_000;
    f64::from(mixed) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn missing_ack_is_a_zombie() {
        let mut heartbeat = Heartbeat::start(Duration::from_secs(10));
        assert!(heartbeat.next() <= Instant::now() + Duration::from_secs(10));
        assert!(matches!(heartbeat.due(), Beat::Send));
        assert!(matches!(heartbeat.due(), Beat::Zombie));

        heartbeat.acked();
        assert!(matches!(heartbeat.due(), Beat::Send));
        assert_eq!(heartbeat.next(), Instant::now() + Duration::from_secs(10));
    }

    #[test]
    fn jitter_is_in_range() {
        for _ in 0..100 {
            let j = jitter();
            assert!((0.0..1.0).contains(&j));
        }
    }
}
