//! Slows down guessing passwords: every failed login is answered late, and a client that
//! fails too often in a row is locked out for a while
use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// Failed logins in a row that lock a client out
    pub max_failures: u32,
    /// Failures further apart than this do not add up
    pub window: Duration,
    /// How long a client stays locked out
    pub lockout: Duration,
    /// How late a failed login is answered
    pub delay: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_failures: 5,
            window: Duration::from_secs(15 * 60),
            lockout: Duration::from_secs(15 * 60),
            delay: Duration::from_secs(1),
        }
    }
}

/// So a flood of clients can not grow the table without end
const MAX_TRACKED_CLIENTS: usize = 10_000;

#[derive(Debug, Clone, Copy)]
struct Failures {
    count: u32,
    last: Instant,
    locked_until: Option<Instant>,
}

pub struct Throttle {
    policy: Policy,
    failures: Mutex<HashMap<IpAddr, Failures>>,
}

impl Throttle {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            failures: Mutex::new(HashMap::new()),
        }
    }

    pub fn delay(&self) -> Duration {
        self.policy.delay
    }

    /// `Err` with the time left while `client` is locked out
    pub fn check(&self, client: IpAddr, now: Instant) -> Result<(), Duration> {
        match self
            .failures
            .lock()
            .get(&client)
            .and_then(|f| f.locked_until)
        {
            Some(until) if until > now => Err(until - now),
            _ => Ok(()),
        }
    }

    /// Count a failed login of `client`, `true` if it locked the client out
    pub fn failure(&self, client: IpAddr, now: Instant) -> bool {
        let policy = self.policy;
        let mut failures = self.failures.lock();
        failures.retain(|_, f| {
            now.duration_since(f.last) < policy.window || f.locked_until.is_some_and(|u| u > now)
        });
        if failures.len() >= MAX_TRACKED_CLIENTS && !failures.contains_key(&client) {
            if let Some(oldest) = failures
                .iter()
                .min_by_key(|(_, f)| f.last)
                .map(|(ip, _)| *ip)
            {
                failures.remove(&oldest);
            }
        }

        let entry = failures.entry(client).or_insert(Failures {
            count: 0,
            last: now,
            locked_until: None,
        });
        if entry.locked_until.is_some_and(|until| until <= now) {
            // the lockout is over, the client starts afresh
            *entry = Failures {
                count: 0,
                last: now,
                locked_until: None,
            };
        }
        entry.count += 1;
        entry.last = now;
        if entry.count >= policy.max_failures {
            entry.locked_until = Some(now + policy.lockout);
            true
        } else {
            false
        }
    }

    /// Forget the failures of `client` after it logged in
    pub fn success(&self, client: IpAddr) {
        self.failures.lock().remove(&client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 7));
    const OTHER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 8));

    fn throttle() -> Throttle {
        Throttle::new(Policy::default())
    }

    #[test]
    fn five_failures_in_a_row_lock_the_client_out_for_15_minutes() {
        let throttle = throttle();
        let start = Instant::now();
        for attempt in 1..=4 {
            assert!(!throttle.failure(CLIENT, start), "attempt {attempt}");
            assert!(throttle.check(CLIENT, start).is_ok());
        }
        assert!(throttle.failure(CLIENT, start));
        assert_eq!(
            throttle.check(CLIENT, start),
            Err(Duration::from_secs(15 * 60))
        );
        // another client is not affected
        assert!(throttle.check(OTHER, start).is_ok());
        // and the lockout ends
        let later = start + Duration::from_secs(15 * 60);
        assert!(throttle.check(CLIENT, later).is_ok());
        assert!(
            !throttle.failure(CLIENT, later),
            "starts afresh after the lockout"
        );
    }

    #[test]
    fn a_login_forgets_the_failures() {
        let throttle = throttle();
        let now = Instant::now();
        for _ in 0..4 {
            throttle.failure(CLIENT, now);
        }
        throttle.success(CLIENT);
        assert!(!throttle.failure(CLIENT, now));
    }

    #[test]
    fn failures_far_apart_do_not_add_up() {
        let throttle = throttle();
        let mut now = Instant::now();
        for _ in 0..10 {
            assert!(!throttle.failure(CLIENT, now));
            now += Duration::from_secs(16 * 60);
        }
    }
}
