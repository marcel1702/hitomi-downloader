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

    /// Reserve a login attempt of `client`, `Err` with the time to wait when there is none
    ///
    /// The attempt counts as a failure from here on, until [`Throttle::success`] forgets it:
    /// counted only once the password was checked, parallel requests would all pass before
    /// the first failure is counted, and the lockout would not hold. One that is never
    /// finished, the client went away during the check, stays counted.
    pub fn begin(&self, client: IpAddr, now: Instant) -> Result<(), Duration> {
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
        match entry.locked_until {
            Some(until) if until > now => return Err(until - now),
            Some(_) => {
                // the lockout is over, the client starts afresh
                *entry = Failures {
                    count: 0,
                    last: now,
                    locked_until: None,
                };
            }
            None => {}
        }
        if entry.count >= policy.max_failures {
            // attempts still being checked use up the rest, should they all fail that is
            // the lockout, should one pass the client is free again
            return Err(policy.lockout);
        }
        entry.count += 1;
        entry.last = now;
        Ok(())
    }

    /// A reserved attempt of `client` failed, `true` if that locked the client out
    pub fn failure(&self, client: IpAddr, now: Instant) -> bool {
        let policy = self.policy;
        let mut failures = self.failures.lock();
        // not there if it was dropped to make room meanwhile, then this is its only failure
        let entry = failures.entry(client).or_insert(Failures {
            count: 1,
            last: now,
            locked_until: None,
        });
        entry.last = now;
        if entry.count >= policy.max_failures && entry.locked_until.is_none() {
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

    /// A login attempt of `client` that fails, `None` if it was not let through at all
    fn fail(throttle: &Throttle, client: IpAddr, now: Instant) -> Option<bool> {
        throttle.begin(client, now).ok()?;
        Some(throttle.failure(client, now))
    }

    #[test]
    fn five_failures_in_a_row_lock_the_client_out_for_15_minutes() {
        let throttle = throttle();
        let start = Instant::now();
        for attempt in 1..=4 {
            assert_eq!(
                fail(&throttle, CLIENT, start),
                Some(false),
                "attempt {attempt}"
            );
        }
        assert_eq!(fail(&throttle, CLIENT, start), Some(true));
        assert_eq!(
            throttle.begin(CLIENT, start),
            Err(Duration::from_secs(15 * 60))
        );
        // another client is not affected
        assert!(throttle.begin(OTHER, start).is_ok());
        // and the lockout ends, the client starts afresh
        let later = start + Duration::from_secs(15 * 60);
        assert_eq!(fail(&throttle, CLIENT, later), Some(false));
    }

    #[test]
    fn a_login_forgets_the_failures() {
        let throttle = throttle();
        let now = Instant::now();
        for _ in 0..4 {
            fail(&throttle, CLIENT, now);
        }
        throttle.begin(CLIENT, now).unwrap();
        throttle.success(CLIENT);
        assert_eq!(fail(&throttle, CLIENT, now), Some(false));
    }

    #[test]
    fn failures_far_apart_do_not_add_up() {
        let throttle = throttle();
        let mut now = Instant::now();
        for _ in 0..10 {
            assert_eq!(fail(&throttle, CLIENT, now), Some(false));
            now += Duration::from_secs(16 * 60);
        }
    }

    #[test]
    fn parallel_attempts_get_no_more_tries_than_one_after_the_other() {
        let throttle = std::sync::Arc::new(throttle());
        let now = Instant::now();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(20));
        let let_through: usize = (0..20)
            .map(|_| {
                let (throttle, barrier) = (throttle.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    usize::from(throttle.begin(CLIENT, now).is_ok())
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .sum();
        assert_eq!(let_through, 5);
        // while they are checked, the client waits, and their failures lock it out
        assert!(throttle.begin(CLIENT, now).is_err());
        let locked: Vec<bool> = (0..5).map(|_| throttle.failure(CLIENT, now)).collect();
        assert_eq!(locked, [true, false, false, false, false]);
        assert_eq!(
            throttle.begin(CLIENT, now),
            Err(Duration::from_secs(15 * 60))
        );
    }

    #[test]
    fn an_attempt_that_passes_frees_the_client_of_the_others() {
        let throttle = throttle();
        let now = Instant::now();
        for _ in 0..5 {
            throttle.begin(CLIENT, now).unwrap();
        }
        assert!(throttle.begin(CLIENT, now).is_err());
        throttle.success(CLIENT);
        assert!(throttle.begin(CLIENT, now).is_ok());
    }
}
