//! How much shell output an SSH Session lets the server send ahead: the shell channel's SSH
//! window.
//!
//! Everything the server has sent and Komando has not yet shown is on its way and can't be
//! called back. Ctrl+C stops the program on the server, but what it printed before still has to
//! cross the link and scroll past; a typed key's echo waits behind it too. On a link slower
//! than the server prints, that output piles up in a queue at the slowest hop. A fixed 8 MiB
//! window kept 0.4 s of output queued at 100 Mbit/s and 3.8 s at 10 Mbit/s, all of which still
//! scrolled past after Ctrl+C. Too small a window, though, leaves a fast distant link idle
//! between round trips: 2 MiB held a 1 Gbit/s link with 50 ms round trips to 20 MB/s.
//!
//! So the window follows the delay output sees, in the manner of LEDBAT (RFC 6817) and of the
//! SFTP flow control (`sftp::flow`). While output arrives, Komando sends SSH pings, one at a
//! time. The server answers a ping after the output it has already sent, so a ping's round trip
//! is the link's own delay plus the time the output queued ahead of it takes to drain, wherever
//! it waits: in the network, or in Komando's buffers when the terminal can't parse it as fast as
//! it comes. The lowest round trip seen recently is the link's own delay; anything above it is
//! queueing. While the queueing stays under the target and the server is using the window, the
//! window grows; above it, it shrinks.
//!
//! The window only grows while output fills it: a program printing slowly doesn't need more,
//! and a window grown while idle would let the next flood queue up at once.
//!
//! The window never drops below [`FLOOR`] worth of the output rate seen lately. When the
//! terminal, not the link, is what holds output back (a busy PC drawing a flood), pings queue
//! behind output Komando has already taken in, which no window can shorten (russh holds up to
//! 100 packets per channel ahead of the parser). Shrinking the window then only makes the
//! server wait for room once per round trip, and with the PC busy, those round trips are slow:
//! the whole app showed 5 to 12 MB/s instead of 25 MB/s. On a network link the floor is about
//! one round trip's worth, which the window needs anyway to keep the link busy.
//!
//! A ping sent during a flood already has output queued ahead of it, so it can't tell the
//! link's own delay. Pings are sent while the link is quiet too: when the shell starts, and when
//! a key is typed after output has paused, which is how a flood usually starts.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The queueing delay shell output may build up: about how much longer than the link's own
/// round trip output keeps coming after Ctrl+C, and a key's echo waits during a flood. On a
/// slow link it is at least one SSH packet's time on the link (26 ms at 10 Mbit/s), as the
/// server sends output a packet at a time.
const TARGET: Duration = Duration::from_millis(20);
/// The largest SSH packet the server sends (russh's default maximum packet size).
const PACKET: u32 = 32 * 1024;
/// Never less than this: two packets' worth.
const MIN_WINDOW: u32 = 2 * PACKET;
/// Never more: enough to keep a 1 Gbit/s link with 50 ms round trips busy.
const MAX_WINDOW: u32 = 8 * 1024 * 1024;
/// The window a shell starts with: a fifth of a second at 10 Mbit/s, and doubled every round
/// trip from the first flood on while the link takes it.
const START_WINDOW: u32 = 256 * 1024;
/// Pings are at most this frequent, so a fast local link isn't flooded with them.
const PING_INTERVAL: Duration = Duration::from_millis(5);
/// Output paused this long counts as a quiet link, for pings that learn its own delay.
const QUIET: Duration = Duration::from_millis(500);
/// How long one lowest-delay record covers, and how many are kept: the link's own delay is the
/// lowest of the last five minutes. Longer than for SFTP: a flood can last minutes, and a base
/// learned only during one would include its queue.
const BASE_INTERVAL: Duration = Duration::from_secs(30);
const BASE_INTERVALS: usize = 10;
/// The queueing taken as current is the lowest of this many latest round trips, as LEDBAT
/// does, so one ping delayed by something else (a busy CPU, Wi-Fi) doesn't shrink the window.
const CURRENT_SAMPLES: usize = 3;
/// The window holds at least this much of the highest output rate seen in the last
/// [`RATE_MEMORY`].
const FLOOR: Duration = Duration::from_millis(50);
const RATE_MEMORY: Duration = Duration::from_secs(2);
/// The most the window grows (or, short of halving, shrinks) per round trip, as a fraction.
const GAIN: f64 = 0.25;

pub(crate) struct Pace {
    window: u32,
    /// Doubling every round trip until queueing shows, as at the start of a TCP connection.
    slow_start: bool,
    /// Lowest round trip per interval, newest last.
    base: VecDeque<(Instant, Duration)>,
    /// The latest round trips, newest last.
    current: VecDeque<Duration>,
    /// Output rates (bytes per second) of the latest round trips, newest last.
    rates: VecDeque<(Instant, f64)>,
    /// When the window was last halved, so it is halved at most once per round trip.
    last_halved: Option<Instant>,
    /// Output bytes received so far, and when the last arrived.
    received: u64,
    last_output: Option<Instant>,
    /// The ping on its way: when it was sent, and `received` then.
    ping: Option<(Instant, u64)>,
    last_ping: Option<Instant>,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            window: START_WINDOW,
            slow_start: true,
            base: VecDeque::new(),
            current: VecDeque::new(),
            rates: VecDeque::new(),
            last_halved: None,
            received: 0,
            last_output: None,
            ping: None,
            last_ping: None,
        }
    }
}

impl Pace {
    /// The window to keep the shell channel at.
    pub(crate) fn window(&self) -> u32 {
        self.window
    }

    /// Whether to send a ping now; if so, it counts as sent. One is on its way at a time.
    fn ping_now(&mut self, now: Instant) -> bool {
        if self.ping.is_some() || self.last_ping.is_some_and(|at| now.duration_since(at) < PING_INTERVAL) {
            return false;
        }
        self.ping = Some((now, self.received));
        self.last_ping = Some(now);
        true
    }

    /// The shell started: ping, to learn the link's own delay while it is quiet.
    pub(crate) fn started(&mut self, now: Instant) -> bool {
        self.ping_now(now)
    }

    /// `len` bytes of output arrived. Returns whether to ping.
    pub(crate) fn output(&mut self, len: usize, now: Instant) -> bool {
        self.received += len as u64;
        self.last_output = Some(now);
        self.ping_now(now)
    }

    /// A key was typed. Returns whether to ping: only while output is paused, when the ping
    /// shows the link's own delay.
    pub(crate) fn typed(&mut self, now: Instant) -> bool {
        self.last_output.is_none_or(|at| now.duration_since(at) >= QUIET) && self.ping_now(now)
    }

    /// The ping came back; `false` if it never will (the connection is closing).
    pub(crate) fn answered(&mut self, answered: bool, now: Instant) {
        if let Some((sent, received)) = self.ping.take()
            && answered
        {
            self.sample(now.duration_since(sent), self.received - received, now);
        }
    }

    /// A ping came back after `rtt`, and `received` bytes of output arrived meanwhile.
    fn sample(&mut self, rtt: Duration, received: u64, now: Instant) {
        match self.base.back_mut() {
            Some((since, lowest)) if now.duration_since(*since) < BASE_INTERVAL => *lowest = (*lowest).min(rtt),
            _ => {
                self.base.push_back((now, rtt));
                if self.base.len() > BASE_INTERVALS {
                    self.base.pop_front();
                }
            }
        }
        self.current.push_back(rtt);
        if self.current.len() > CURRENT_SAMPLES {
            self.current.pop_front();
        }
        let base = self.base.iter().map(|(_, d)| *d).min().unwrap_or(rtt);
        let current = self.current.iter().min().copied().unwrap_or(rtt);
        let queueing = current.saturating_sub(base);
        // The server keeps nearly all of the window on its way (it is topped up after every
        // eighth used, see `vendor/russh`), so a quarter of it arriving per round trip means
        // the window is what holds it back, or soon will.
        let limited = received >= self.window as u64 / 4;
        // While it is, what arrived per round trip is about the link's rate, and a packet
        // takes PACKET / rate to cross it.
        let target = if limited {
            TARGET.max(Duration::from_secs_f64(PACKET as f64 * rtt.as_secs_f64() / received as f64))
        } else {
            TARGET
        };

        let mut window = self.window as f64;
        if self.slow_start {
            if queueing >= target / 2 {
                self.slow_start = false;
            } else if limited {
                window *= 2.0;
            }
        }
        if !self.slow_start {
            if queueing > target * 2 {
                // Far over: the link got slower, or something else is using it. Halve, once
                // per round trip, rather than wait for the gentle decrease below.
                if self.last_halved.is_none_or(|at| now.duration_since(at) > rtt) {
                    window /= 2.0;
                    self.last_halved = Some(now);
                }
            } else {
                // In proportion to how far from the target, up to GAIN per round trip.
                let off = ((target.as_secs_f64() - queueing.as_secs_f64()) / target.as_secs_f64()).clamp(-1.0, 1.0);
                if off < 0.0 || limited {
                    window *= 1.0 + GAIN * off;
                }
            }
        }
        self.rates.push_back((now, received as f64 / rtt.as_secs_f64().max(1e-4)));
        while self.rates.front().is_some_and(|(at, _)| now.duration_since(*at) > RATE_MEMORY) {
            self.rates.pop_front();
        }
        let rate = self.rates.iter().map(|(_, rate)| *rate).fold(0.0, f64::max);
        let window = window.max(rate * FLOOR.as_secs_f64());
        self.window = (window as u32).clamp(MIN_WINDOW, MAX_WINDOW);
        #[cfg(test)]
        if std::env::var_os("KOMANDO_PACE_DEBUG").is_some() {
            eprintln!("pace: rtt {rtt:?} received {received} queueing {queueing:?} target {target:?} -> window {}", self.window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flood through a link of `rate` bytes/s and `delay` round trip, with a queue in front
    /// of it, for `seconds`, with the server always using all of the window it has (fifteen
    /// sixteenths of it on average, see `sample`). Returns the window and the queueing delay at
    /// the end.
    fn simulate(rate: f64, delay: Duration, seconds: f64) -> (u32, Duration) {
        let mut pace = Pace::default();
        let start = Instant::now();
        // The ping when the shell starts, on a quiet link.
        pace.sample(delay, 0, start);
        let mut now = 0.0;
        let mut queueing = 0.0;
        while now < seconds {
            let in_flight = pace.window() as f64 * 0.94;
            // What the link holds itself; the rest waits in the queue.
            queueing = ((in_flight - rate * delay.as_secs_f64()) / rate).max(0.0);
            let rtt = delay.as_secs_f64() + queueing;
            let received = rate.min(in_flight / rtt) * rtt;
            now += rtt.max(PING_INTERVAL.as_secs_f64());
            pace.sample(Duration::from_secs_f64(rtt), received as u64, start + Duration::from_secs_f64(now));
        }
        (pace.window(), Duration::from_secs_f64(queueing))
    }

    #[test]
    fn a_slow_link_gets_a_short_queue() {
        // 10 Mbit/s, 30 ms: 37.5 KB is enough to keep it busy, and a packet takes 26 ms.
        let (window, queue) = simulate(1.25e6, Duration::from_millis(30), 20.0);
        assert!(queue <= Duration::from_millis(60), "queue {queue:?}, window {window}");
        // 100 Mbit/s, 50 ms.
        let (window, queue) = simulate(12.5e6, Duration::from_millis(50), 20.0);
        assert!(queue <= TARGET * 2, "queue {queue:?}, window {window}");
        assert!(window < 2 * 1024 * 1024, "window {window}");
    }

    #[test]
    fn a_fast_distant_link_gets_a_large_window() {
        // 1 Gbit/s, 50 ms: 6.25 MB in flight to keep it busy.
        let (window, _) = simulate(125e6, Duration::from_millis(50), 2.0);
        assert!(window >= 7 * 1024 * 1024, "window {window}");
    }

    #[test]
    fn output_that_does_not_fill_the_window_does_not_grow_it() {
        let mut pace = Pace::default();
        let start = Instant::now();
        for i in 0..100 {
            // A few lines per round trip, no queueing.
            pace.sample(Duration::from_millis(50), 2_000, start + Duration::from_millis(50 * i));
        }
        assert_eq!(pace.window(), START_WINDOW);
    }

    #[test]
    fn one_slow_ping_does_not_shrink_the_window() {
        let mut pace = Pace { window: MAX_WINDOW, slow_start: false, ..Default::default() };
        let start = Instant::now();
        pace.sample(Duration::from_millis(50), MAX_WINDOW as u64 / 4, start);
        pace.sample(Duration::from_millis(150), MAX_WINDOW as u64 / 4, start + Duration::from_millis(150));
        assert_eq!(pace.window(), MAX_WINDOW);
    }

    #[test]
    fn a_terminal_that_lags_keeps_the_window_it_needs() {
        // A local link (0.2 ms) whose terminal takes in 25 MB/s, with 3 MB queued ahead of the
        // parser whatever the window: every ping waits 120 ms.
        let mut pace = Pace::default();
        let start = Instant::now();
        pace.sample(Duration::from_micros(200), 0, start);
        let mut now = Duration::ZERO;
        for _ in 0..100 {
            now += Duration::from_millis(120);
            pace.sample(Duration::from_millis(120), 3_000_000, start + now);
        }
        assert!(pace.window() as f64 >= 25e6 * 0.05 * 0.99, "window {}", pace.window());
    }

    #[test]
    fn pings_go_one_at_a_time_and_learn_the_delay_of_a_quiet_link() {
        let mut pace = Pace::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        assert!(pace.started(at(0)));
        assert!(!pace.output(100, at(10)), "one ping at a time");
        pace.answered(true, at(30));
        assert_eq!(pace.base.back().map(|b| b.1), Some(Duration::from_millis(30)));
        assert!(!pace.typed(at(100)), "output came too recently");
        assert!(pace.typed(at(600)), "output paused");
        pace.answered(true, at(625));
        assert_eq!(pace.base.back().map(|b| b.1), Some(Duration::from_millis(25)));
        // During output, a ping per round trip, at most one per PING_INTERVAL.
        assert!(pace.output(32_768, at(700)));
        pace.answered(true, at(701));
        assert!(!pace.output(32_768, at(702)));
        assert!(pace.output(32_768, at(706)));
        // A ping that never comes back says nothing, and frees the way for the next.
        pace.answered(false, at(800));
        assert!(pace.output(32_768, at(810)));
    }

    #[test]
    fn a_queue_far_over_the_target_halves_the_window_once_per_round_trip() {
        let mut pace = Pace { window: MAX_WINDOW, slow_start: false, ..Default::default() };
        let start = Instant::now();
        pace.sample(Duration::from_millis(30), 0, start);
        for ms in [10, 20, 30] {
            pace.sample(Duration::from_millis(500), 0, start + Duration::from_millis(ms));
        }
        assert_eq!(pace.window(), MAX_WINDOW / 2);
        pace.sample(Duration::from_millis(500), 0, start + Duration::from_millis(40));
        assert_eq!(pace.window(), MAX_WINDOW / 2, "not twice within one round trip");
        pace.sample(Duration::from_millis(500), 0, start + Duration::from_millis(600));
        assert_eq!(pace.window(), MAX_WINDOW / 4);
    }
}
