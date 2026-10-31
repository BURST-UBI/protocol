use std::time::Duration;
use tokio::time::{Interval, MissedTickBehavior};

/// Create a repair timer with an immediate first tick and skipped missed ticks.
/// Repair reads current durable work; replaying every missed poll adds no value.
/// Callers select separately on event notifications.
pub(super) fn interval(period: Duration) -> Interval {
    let mut timer = tokio::time::interval(period);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    timer
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };

    #[tokio::test]
    async fn overdue_repairs_coalesce_without_consuming_event_notifications() {
        // Keep the next phase far in the future without sleeping or changing
        // the protocol clock. Burst mode would return several overdue ticks.
        let period = Duration::from_secs(60);
        let mut timer = interval(period);
        timer.tick().await;
        for _ in 0..3 {
            timer.reset_at(tokio::time::Instant::now() - period * 4 - period / 2);
            timer.tick().await;
            let mut next = pin!(timer.tick());
            assert!(matches!(
                next.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
        }
        let ready = tokio::sync::Notify::new();
        ready.notify_one();
        tokio::select! {
            biased;
            _ = timer.tick() => panic!("missed repair ticks ran ahead of ready work"),
            _ = ready.notified() => {},
        }
        // Recovery remains available even with no event notification.
        timer.reset_at(tokio::time::Instant::now() - period * 2);
        timer.tick().await;
    }
}
