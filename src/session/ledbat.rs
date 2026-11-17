//! The delay bookkeeping shared by the two LEDBAT-style controllers (RFC 6817): the shell
//! window (`session::pace`) and SFTP transfers (`sftp::flow`). Each keeps its own constants and
//! decides for itself what to do with the queueing delay; this only tracks the link's own delay
//! and the once-per-round-trip limit on halving.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub(crate) struct LinkDelay {
    /// Lowest round trip per interval, newest last.
    base: VecDeque<(Instant, Duration)>,
    /// How long one record covers, and how many are kept.
    interval: Duration,
    intervals: usize,
    /// When the window was last halved, so it is halved at most once per round trip.
    last_halved: Option<Instant>,
}

impl LinkDelay {
    pub(crate) fn new(interval: Duration, intervals: usize) -> Self {
        Self { base: VecDeque::new(), interval, intervals, last_halved: None }
    }

    /// Record a round trip of `rtt`. Returns the link's own delay: the lowest round trip of
    /// the records kept.
    pub(crate) fn sample(&mut self, rtt: Duration, now: Instant) -> Duration {
        match self.base.back_mut() {
            Some((since, lowest)) if now.duration_since(*since) < self.interval => *lowest = (*lowest).min(rtt),
            _ => {
                self.base.push_back((now, rtt));
                if self.base.len() > self.intervals {
                    self.base.pop_front();
                }
            }
        }
        self.base.iter().map(|(_, d)| *d).min().unwrap_or(rtt)
    }

    /// Whether the window may be halved now, at most once per round trip (`rtt`); if so, it
    /// counts as halved.
    pub(crate) fn halve(&mut self, rtt: Duration, now: Instant) -> bool {
        let may = self.last_halved.is_none_or(|at| now.duration_since(at) > rtt);
        if may {
            self.last_halved = Some(now);
        }
        may
    }

    /// The newest record: when it began and the lowest round trip in it.
    #[cfg(test)]
    pub(crate) fn back(&self) -> Option<&(Instant, Duration)> {
        self.base.back()
    }
}
