use tokio::sync::{broadcast, mpsc};
use tokio::time::Interval;

pub(super) async fn next<T>(
    stop: &mut broadcast::Receiver<()>,
    inbox: &mut mpsc::Receiver<T>,
    timer: &mut Interval,
) -> Result<Option<T>, ()> {
    // Every packet is followed by a poll in the worker. Timer priority would
    // starve the inbox whenever one replay takes longer than the tick period.
    tokio::select! {
        biased;
        _ = stop.recv() => Err(()),
        packet = inbox.recv() => packet.map(Some).ok_or(()),
        _ = timer.tick() => Ok(None),
    }
}

pub(super) async fn next_with_initial<T>(
    stop: &mut broadcast::Receiver<()>,
    inbox: &mut mpsc::Receiver<T>,
    timer: &mut Interval,
    initial: &mut Option<T>,
) -> Result<Option<T>, ()> {
    if initial.is_some() {
        tokio::select! {
            biased;
            _ = stop.recv() => Err(()),
            _ = std::future::ready(()) => Ok(initial.take()),
        }
    } else {
        next(stop, inbox, timer).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn overdue(timer: &mut Interval) {
        timer.reset_at(tokio::time::Instant::now() - Duration::from_secs(1));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    async fn timer() -> Interval {
        let mut timer = tokio::time::interval(Duration::from_millis(250));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        timer.tick().await;
        overdue(&mut timer).await;
        timer
    }

    #[tokio::test]
    async fn overdue_timer_cannot_starve_queued_agreement_packets() {
        let (_shutdown, mut stop) = broadcast::channel(1);
        let (sender, mut inbox) = mpsc::channel(4);
        let mut timer = timer().await;
        for value in 0..4 {
            sender.send(value).await.unwrap();
        }
        for value in 0..4 {
            overdue(&mut timer).await;
            assert_eq!(
                next(&mut stop, &mut inbox, &mut timer).await,
                Ok(Some(value))
            );
        }
        assert_eq!(next(&mut stop, &mut inbox, &mut timer).await, Ok(None));
    }

    #[tokio::test]
    async fn shutdown_has_priority_over_packets_and_timer() {
        let (shutdown, mut stop) = broadcast::channel(1);
        let (sender, mut inbox) = mpsc::channel(1);
        sender.send(1).await.unwrap();
        shutdown.send(()).unwrap();
        let mut timer = timer().await;
        assert_eq!(next(&mut stop, &mut inbox, &mut timer).await, Err(()));
        assert_eq!(inbox.try_recv().unwrap(), 1);
    }

    #[tokio::test]
    async fn closed_inbox_stops_even_when_timer_is_due() {
        let (_shutdown, mut stop) = broadcast::channel(1);
        let (sender, mut inbox) = mpsc::channel::<u8>(1);
        drop(sender);
        let mut timer = timer().await;
        assert_eq!(next(&mut stop, &mut inbox, &mut timer).await, Err(()));
    }

    #[tokio::test]
    async fn handoff_packet_precedes_the_inbox_exactly_once() {
        let (_shutdown, mut stop) = broadcast::channel(1);
        let (sender, mut inbox) = mpsc::channel(1);
        sender.send(2).await.unwrap();
        let mut initial = Some(1);
        let mut timer = timer().await;
        assert_eq!(
            next_with_initial(&mut stop, &mut inbox, &mut timer, &mut initial).await,
            Ok(Some(1))
        );
        assert!(initial.is_none());
        assert_eq!(
            next_with_initial(&mut stop, &mut inbox, &mut timer, &mut initial).await,
            Ok(Some(2))
        );
    }

    #[tokio::test]
    async fn shutdown_precedes_the_handoff_packet() {
        let (shutdown, mut stop) = broadcast::channel(1);
        let (_sender, mut inbox) = mpsc::channel(1);
        let mut initial = Some(1);
        let mut timer = timer().await;
        shutdown.send(()).unwrap();
        assert_eq!(
            next_with_initial(&mut stop, &mut inbox, &mut timer, &mut initial).await,
            Err(())
        );
        assert_eq!(initial, Some(1));
    }
}
