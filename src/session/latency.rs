//! SSH round trips for the connection indicator. Sampling is independent of shell traffic
//! and never disconnects a Session. Each hop is measured from this computer, through all
//! preceding Jump Hosts, rather than being a delay to add to the other hops.

use std::future::Future;
use std::time::{Duration, Instant};

use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyState {
    Measuring,
    Responsive,
    Slow,
    Unresponsive,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct HopLatency {
    pub hop: usize,
    pub name: String,
    /// An exponentially smoothed round trip: 3/4 previous value, 1/4 new sample.
    pub round_trip: Option<Duration>,
    pub measured_at: Option<Instant>,
    pub state: ReplyState,
}

impl HopLatency {
    fn new(hop: usize, name: String) -> Self {
        Self { hop, name, round_trip: None, measured_at: None, state: ReplyState::Measuring }
    }

    fn answered(&mut self, sample: Duration, now: Instant) {
        self.round_trip = Some(self.round_trip.map_or(sample, |old| old.mul_f64(0.75) + sample.mul_f64(0.25)));
        self.measured_at = Some(now);
        self.state = ReplyState::Responsive;
    }

    pub fn bars(&self) -> u8 {
        match self.state {
            ReplyState::Measuring | ReplyState::Unresponsive | ReplyState::Unavailable => 0,
            ReplyState::Slow => 1,
            ReplyState::Responsive => match self.round_trip {
                Some(rtt) if rtt < Duration::from_millis(30) => 4,
                Some(rtt) if rtt < Duration::from_millis(80) => 3,
                Some(rtt) if rtt < Duration::from_millis(150) => 2,
                Some(_) => 1,
                None => 0,
            },
        }
    }

    pub fn description(&self, now: Instant) -> String {
        let sample = match (self.round_trip, self.measured_at) {
            (Some(rtt), Some(at)) => {
                let latency =
                    if rtt < Duration::from_millis(1) { "<1 ms".into() } else { format!("{} ms", rtt.as_millis()) };
                format!("{latency} · measured {} s ago", now.saturating_duration_since(at).as_secs())
            }
            _ => "No measurement yet".into(),
        };
        match self.state {
            ReplyState::Measuring => "Measuring latency…".into(),
            ReplyState::Responsive => sample,
            ReplyState::Slow => format!("Waiting for reply (over 2 s) · {sample}"),
            ReplyState::Unresponsive => format!("No reply for over 5 s · {sample}"),
            ReplyState::Unavailable => format!("Latency unavailable · {sample}"),
        }
    }
}

/// Overdue replies take precedence over missing samples. Otherwise show the weakest
/// measured hop, so an unavailable Jump Host measurement cannot hide a stalled target.
pub fn weakest_hop(hops: &[HopLatency]) -> Option<&HopLatency> {
    hops.iter().min_by_key(|hop| {
        let priority = match hop.state {
            ReplyState::Unresponsive => 0,
            ReplyState::Slow => 1,
            ReplyState::Responsive => 2,
            ReplyState::Unavailable => 3,
            ReplyState::Measuring => 4,
        };
        (priority, hop.bars())
    })
}

#[derive(Clone, Copy)]
struct Timing {
    active: Duration,
    background: Duration,
    slow: Duration,
    unresponsive: Duration,
}

const TIMING: Timing = Timing {
    active: Duration::from_secs(5),
    background: Duration::from_secs(30),
    slow: Duration::from_secs(2),
    unresponsive: Duration::from_secs(5),
};

/// One outstanding ping at most. A late reply still belongs to that ping, so it can recover
/// the indicator without retries piling up in SSH's ordered global-request reply queue.
/// Dropping this future cancels sampling, including a ping that never answered.
pub(crate) async fn monitor<P, F, R>(hop: usize, name: String, active: watch::Receiver<bool>, ping: P, report: R)
where
    P: FnMut() -> F,
    F: Future<Output = bool>,
    R: FnMut(HopLatency),
{
    monitor_with_timing(HopLatency::new(hop, name), active, ping, report, TIMING).await;
}

async fn monitor_with_timing<P, F, R>(
    mut latency: HopLatency,
    mut active: watch::Receiver<bool>,
    mut ping: P,
    mut report: R,
    timing: Timing,
) where
    P: FnMut() -> F,
    F: Future<Output = bool>,
    R: FnMut(HopLatency),
{
    report(latency.clone());
    loop {
        let started = Instant::now();
        let answer = ping();
        tokio::pin!(answer);
        let slow = tokio::time::sleep(timing.slow);
        let unresponsive = tokio::time::sleep(timing.unresponsive);
        tokio::pin!(slow, unresponsive);
        let mut warned = false;
        let mut stalled = false;
        let ok = loop {
            tokio::select! {
                // A completed reply wins over a deadline when the executor was busy.
                biased;
                ok = &mut answer => break ok,
                _ = &mut slow, if !warned => {
                    warned = true;
                    latency.state = ReplyState::Slow;
                    report(latency.clone());
                }
                _ = &mut unresponsive, if !stalled => {
                    stalled = true;
                    latency.state = ReplyState::Unresponsive;
                    report(latency.clone());
                }
            }
        };
        if !ok {
            latency.state = ReplyState::Unavailable;
            report(latency);
            return;
        }
        let now = Instant::now();
        latency.answered(now.duration_since(started), now);
        report(latency.clone());

        // Activation refreshes immediately; deactivation switches to the longer interval.
        // Changes while a ping was outstanding are coalesced into the current window state.
        loop {
            let foreground = *active.borrow_and_update();
            let interval = if foreground { timing.active } else { timing.background };
            tokio::select! {
                _ = tokio::time::sleep(interval) => break,
                changed = active.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    if *active.borrow_and_update() {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_follow_smoothed_round_trip_and_pending_replies() {
        let now = Instant::now();
        let mut hop = HopLatency::new(0, "host".into());
        assert_eq!(hop.bars(), 0);
        for (ms, bars) in [(0, 4), (29, 4), (30, 3), (79, 3), (80, 2), (149, 2), (150, 1), (2000, 1)] {
            hop.round_trip = None;
            hop.answered(Duration::from_millis(ms), now);
            assert_eq!(hop.bars(), bars, "{ms} ms");
        }
        hop.round_trip = None;
        hop.answered(Duration::from_millis(20), now);
        hop.answered(Duration::from_millis(100), now);
        assert_eq!(hop.round_trip, Some(Duration::from_millis(40)));
        assert_eq!(hop.bars(), 3);
        hop.state = ReplyState::Slow;
        assert_eq!(hop.bars(), 1);
        hop.state = ReplyState::Unresponsive;
        assert_eq!(hop.bars(), 0);
        hop.answered(Duration::from_millis(40), now);
        assert_eq!(hop.bars(), 3);
    }

    #[test]
    fn tooltip_keeps_the_last_sample_and_its_age_when_replies_stop() {
        let now = Instant::now();
        let mut hop = HopLatency::new(1, "target".into());
        hop.answered(Duration::from_micros(500), now);
        assert_eq!(hop.description(now + Duration::from_secs(12)), "<1 ms · measured 12 s ago");
        hop.state = ReplyState::Unresponsive;
        assert!(
            hop.description(now + Duration::from_secs(40))
                .contains("No reply for over 5 s · <1 ms · measured 40 s ago")
        );
    }

    #[test]
    fn missing_hop_samples_cannot_hide_an_overdue_reply() {
        let mut jump = HopLatency::new(0, "jump".into());
        jump.state = ReplyState::Unavailable;
        let mut target = HopLatency::new(1, "target".into());
        target.state = ReplyState::Unresponsive;
        assert_eq!(weakest_hop(&[jump, target]).unwrap().state, ReplyState::Unresponsive);
        assert!(weakest_hop(&[]).is_none());
    }

    const TEST_TIMING: Timing = Timing {
        active: Duration::from_millis(20),
        background: Duration::from_secs(5),
        slow: Duration::from_millis(20),
        unresponsive: Duration::from_millis(40),
    };

    async fn next(rx: &mut tokio::sync::mpsc::UnboundedReceiver<HopLatency>) -> HopLatency {
        tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn unanswered_ping_is_bounded_and_a_late_reply_recovers() {
        let (_focus, active) = watch::channel(true);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (reply, answer) = tokio::sync::oneshot::channel();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let mut answer = Some(answer);
        let task = tokio::spawn(monitor_with_timing(
            HopLatency::new(1, "target".into()),
            active,
            move || {
                count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let answer = answer.take();
                async move {
                    match answer {
                        Some(answer) => answer.await.unwrap_or(false),
                        None => std::future::pending().await,
                    }
                }
            },
            move |sample| {
                tx.send(sample).unwrap();
            },
            TEST_TIMING,
        ));
        assert_eq!(next(&mut rx).await.state, ReplyState::Measuring);
        assert_eq!(next(&mut rx).await.state, ReplyState::Slow);
        assert_eq!(next(&mut rx).await.state, ReplyState::Unresponsive);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        reply.send(true).unwrap();
        let recovered = next(&mut rx).await;
        assert_eq!(recovered.state, ReplyState::Responsive);
        assert_eq!(recovered.hop, 1);
        assert_eq!(recovered.name, "target");
        task.abort();
        let _ = task.await;
        // Cancelling a Session also drops the outstanding monitor and its event sender.
        while rx.recv().await.is_some() {}
    }

    #[tokio::test]
    async fn foreground_refreshes_background_sampling_without_a_second_pending_ping() {
        let (focus, active) = watch::channel(false);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(monitor_with_timing(
            HopLatency::new(0, "jump".into()),
            active,
            || async { true },
            move |sample| {
                tx.send(sample).unwrap();
            },
            TEST_TIMING,
        ));
        assert_eq!(next(&mut rx).await.state, ReplyState::Measuring);
        assert_eq!(next(&mut rx).await.state, ReplyState::Responsive);
        assert!(tokio::time::timeout(Duration::from_millis(70), rx.recv()).await.is_err());
        focus.send_replace(true);
        assert_eq!(next(&mut rx).await.state, ReplyState::Responsive);
        assert_eq!(next(&mut rx).await.state, ReplyState::Responsive);
        focus.send_replace(false);
        // A ping that completed just before deactivation may already have reported.
        tokio::time::sleep(Duration::from_millis(50)).await;
        while rx.try_recv().is_ok() {}
        assert!(tokio::time::timeout(Duration::from_millis(70), rx.recv()).await.is_err());
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn failed_ping_stops_sampling_without_reporting_a_latency() {
        let (_focus, active) = watch::channel(true);
        let mut reports = Vec::new();
        monitor_with_timing(
            HopLatency::new(0, "host".into()),
            active,
            || async { false },
            |sample| reports.push(sample),
            TEST_TIMING,
        )
        .await;
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[1].state, ReplyState::Unavailable);
        assert!(reports[1].round_trip.is_none());
    }
}
