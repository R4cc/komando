//! How much transfer data a connection keeps on its way at once, in one direction: downloads
//! and uploads each have their own [`Flow`].
//!
//! Every byte asked for but not yet received (or sent but not yet confirmed) sits somewhere
//! between the server and Komando, and on a slow link mostly in a queue at the slowest hop.
//! Keystrokes and their echoes on the same connection wait behind that queue: with a fixed 4 MB
//! in flight, a 100 Mbit/s link added a third of a second to every key. Too little in flight,
//! though, leaves a fast distant link idle between round trips.
//!
//! So the amount follows the delay each read sees, in the manner of LEDBAT (RFC 6817): the
//! lowest round trip seen recently is the link's own delay; anything above it is queueing. While
//! the queueing delay stays under [`TARGET`], more is allowed in flight; above it, less. A
//! keystroke then waits about [`TARGET`] longer than on an idle connection, while the link
//! stays busy.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The queueing delay transfers may add to everything else on the connection.
pub(crate) const TARGET: Duration = Duration::from_millis(20);
/// The largest read: OpenSSH's limit (256 KiB packets, less headers).
pub(crate) const MAX_REQUEST: u32 = 255 * 1024;
/// The smallest read, so a slow link isn't flooded with tiny requests.
const MIN_REQUEST: u32 = 16 * 1024;
/// Never less than this in flight.
const MIN_WINDOW: u64 = 64 * 1024;
/// Never more: the SSH window (`window_size` in `session::ssh`) stops the server there anyway.
const MAX_WINDOW: u64 = 8 * 1024 * 1024;
const START_WINDOW: u64 = 128 * 1024;
/// How long one lowest-delay record covers, and how many are kept: the link's own delay is
/// the lowest of the last minute, so a route that gets slower is learned within a minute.
const BASE_INTERVAL: Duration = Duration::from_secs(10);
const BASE_INTERVALS: usize = 6;

/// One connection's downloads share one of these, and its uploads another.
pub(crate) struct Flow {
    state: Mutex<State>,
    room: tokio::sync::Notify,
}

struct State {
    /// Bytes allowed in flight.
    window: u64,
    in_flight: u64,
    /// Doubling every round trip until queueing shows, as at the start of a TCP connection.
    slow_start: bool,
    /// Lowest round trip per interval, newest last.
    base: VecDeque<(Instant, Duration)>,
    /// When the window was last halved, so it is halved at most once per round trip.
    last_halved: Option<Instant>,
}

impl Default for Flow {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                window: START_WINDOW,
                in_flight: 0,
                slow_start: true,
                base: VecDeque::new(),
                last_halved: None,
            }),
            room: tokio::sync::Notify::new(),
        }
    }
}

impl Flow {
    /// Room for one more read: its length, reserved until [`Flow::done`]. `None` while the
    /// window is full. One read may always be in flight.
    pub(crate) fn try_reserve(&self, at_most: u32) -> Option<u32> {
        let mut state = self.lock();
        let len = request_len(state.window).min(at_most.max(1));
        if state.in_flight > 0 && state.in_flight + len as u64 > state.window {
            return None;
        }
        state.in_flight += len as u64;
        Some(len)
    }

    /// Room for one more read, waiting for it if the window is full.
    pub(crate) async fn reserve(&self, at_most: u32) -> u32 {
        loop {
            // Listening before looking, so room made in between isn't missed.
            let notified = self.room.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(len) = self.try_reserve(at_most) {
                return len;
            }
            notified.await;
        }
    }

    /// Wait until something in flight arrives (or is given up), which may make room.
    pub(crate) async fn room(&self) {
        self.room.notified().await
    }

    /// A read of `len` bytes finished after `rtt`. `rtt` is `None` for a read that failed,
    /// which says nothing about the link.
    pub(crate) fn done(&self, len: u32, rtt: Option<Duration>) {
        {
            let mut state = self.lock();
            state.in_flight = state.in_flight.saturating_sub(len as u64);
            if let Some(rtt) = rtt {
                state.sample(len, rtt, Instant::now());
            }
        }
        self.room.notify_waiters();
    }

    /// Bytes currently allowed in flight.
    #[cfg(test)]
    pub(crate) fn window(&self) -> u64 {
        self.lock().window
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Reads a sixteenth of the window each, within the limits. Small next to the window, because
/// a read's round trip includes the time its own data takes to cross the link: with large
/// reads that looks like queueing, and the window stays smaller than the link could take.
fn request_len(window: u64) -> u32 {
    (window / 16).clamp(MIN_REQUEST as u64, MAX_REQUEST as u64) as u32
}

impl State {
    fn sample(&mut self, len: u32, rtt: Duration, now: Instant) {
        match self.base.back_mut() {
            Some((since, lowest)) if now.duration_since(*since) < BASE_INTERVAL => *lowest = (*lowest).min(rtt),
            _ => {
                self.base.push_back((now, rtt));
                if self.base.len() > BASE_INTERVALS {
                    self.base.pop_front();
                }
            }
        }
        let base = self.base.iter().map(|(_, d)| *d).min().unwrap_or(rtt);
        let queueing = rtt.saturating_sub(base);

        if self.slow_start {
            if queueing < TARGET / 2 {
                self.window += len as u64;
            } else {
                self.slow_start = false;
            }
        } else if queueing > TARGET * 2 {
            // Far over: the link got slower, or something else is using it. Halve, once per
            // round trip, rather than wait for the gentle decrease below.
            if self.last_halved.is_none_or(|at| now.duration_since(at) > rtt) {
                self.window /= 2;
                self.last_halved = Some(now);
            }
        } else {
            // Up to one read's worth more (or less) per window's worth of data received:
            // about one read per round trip, in proportion to how far from the target.
            let off = (TARGET.as_secs_f64() - queueing.as_secs_f64()) / TARGET.as_secs_f64();
            let step = off.clamp(-1.0, 1.0) * len as f64 * request_len(self.window) as f64 / self.window as f64;
            self.window = (self.window as f64 + step).max(0.0) as u64;
        }
        self.window = self.window.clamp(MIN_WINDOW, MAX_WINDOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Downloads through a link of `rate` bytes/s and `delay` round trip, with a queue in
    /// front of it, for `seconds`. Returns the window and the average queueing delay over the
    /// last second.
    fn simulate(rate: f64, delay: Duration, seconds: f64) -> (u64, Duration) {
        let flow = Flow::default();
        let start = Instant::now();
        // Reads in flight: (len, sent at, arrives at), in order. The link sends one at a time.
        let mut flight: VecDeque<(u32, f64, f64)> = VecDeque::new();
        let mut link_free = 0.0f64;
        let mut now = 0.0f64;
        let mut late = vec![];
        while now < seconds {
            while let Some(len) = flow.try_reserve(MAX_REQUEST) {
                // The request reaches the server after half the delay; the reply queues for
                // the link, then takes the other half.
                let at_server = now + delay.as_secs_f64() / 2.0;
                link_free = link_free.max(at_server) + len as f64 / rate;
                flight.push_back((len, now, link_free + delay.as_secs_f64() / 2.0));
            }
            let (len, sent, arrives) = flight.pop_front().unwrap();
            now = arrives;
            let rtt = Duration::from_secs_f64(arrives - sent);
            {
                let mut state = flow.lock();
                state.in_flight -= len as u64;
                state.sample(len, rtt, start + Duration::from_secs_f64(now));
            }
            if now > seconds - 1.0 {
                // What a keystroke sent now would wait for when it reaches the server: the
                // replies to everything asked for before it, still queued for the link.
                late.push((link_free - (now + delay.as_secs_f64() / 2.0)).max(0.0));
            }
        }
        let average = late.iter().sum::<f64>() / late.len().max(1) as f64;
        (flow.window(), Duration::from_secs_f64(average))
    }

    #[test]
    fn a_slow_link_gets_a_short_queue() {
        // 100 Mbit/s, 50 ms: 625 KB is enough to keep it busy.
        let (window, queue) = simulate(12.5e6, Duration::from_millis(50), 20.0);
        assert!(queue < TARGET * 2, "queue {queue:?}, window {window}");
        assert!(window < 2 * 1024 * 1024, "window {window}");
    }

    #[test]
    fn a_fast_distant_link_gets_a_large_window() {
        // 1 Gbit/s, 50 ms: 6.25 MB in flight to keep it busy.
        let (window, _) = simulate(125e6, Duration::from_millis(50), 20.0);
        assert!(window >= 6 * 1024 * 1024, "window {window}");
    }

    #[test]
    fn a_very_slow_link_uses_small_reads() {
        // 8 Mbit/s, 30 ms.
        let (window, queue) = simulate(1e6, Duration::from_millis(30), 30.0);
        assert!(queue < TARGET * 3, "queue {queue:?}, window {window}");
        assert_eq!(request_len(window), MIN_REQUEST);
    }

    #[test]
    fn one_read_may_always_be_in_flight() {
        let flow = Flow::default();
        flow.lock().window = MIN_WINDOW;
        assert!(flow.try_reserve(MAX_REQUEST).is_some());
        let second = flow.try_reserve(MAX_REQUEST);
        assert!(second.is_some(), "64 KiB holds four 16 KiB reads");
        assert!(flow.try_reserve(MAX_REQUEST).is_some());
        assert!(flow.try_reserve(MAX_REQUEST).is_some());
        assert!(flow.try_reserve(MAX_REQUEST).is_none());
        for _ in 0..4 {
            flow.done(MIN_REQUEST, None);
        }
        assert!(flow.try_reserve(10).is_some_and(|len| len == 10), "never more than asked for");
    }
}
