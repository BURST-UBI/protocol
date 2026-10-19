use tokio::sync::{broadcast, mpsc};
use tokio::time::Interval;

#[derive(Default)]
pub(super) struct Scheduler {
    packet_turn: bool,
}

impl Scheduler {
    pub(super) async fn next<T>(
        &mut self,
        stop: &mut broadcast::Receiver<()>,
        inbox: &mut mpsc::Receiver<T>,
        timer: &mut Interval,
    ) -> Result<Option<T>, ()> {
        // Polls may outlast the interval; packets may arrive continuously.
        // Alternate priority so neither source can starve the other.
        let next = if self.packet_turn {
            tokio::select! {
                biased;
                _ = stop.recv() => Err(()),
                packet = inbox.recv() => packet.map(Some).ok_or(()),
                _ = timer.tick() => Ok(None),
            }
        } else {
            tokio::select! {
                biased;
                _ = stop.recv() => Err(()),
                _ = timer.tick() => Ok(None),
                packet = inbox.recv() => packet.map(Some).ok_or(()),
            }
        };
        self.packet_turn = matches!(next, Ok(None));
        next
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
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        timer.tick().await;
        overdue(&mut timer).await;
        timer
    }

    #[tokio::test]
    async fn slow_polls_and_busy_inbox_both_make_progress() {
        let (_shutdown, mut stop) = broadcast::channel(1);
        let (sender, mut inbox) = mpsc::channel(4);
        let mut timer = timer().await;
        let mut scheduler = Scheduler::default();
        for packet in 0..4 {
            sender.send(packet).await.unwrap();
        }
        for packet in 0..4 {
            overdue(&mut timer).await;
            assert_eq!(
                scheduler.next(&mut stop, &mut inbox, &mut timer).await,
                Ok(None)
            );
            overdue(&mut timer).await;
            assert_eq!(
                scheduler.next(&mut stop, &mut inbox, &mut timer).await,
                Ok(Some(packet))
            );
        }
    }

    #[tokio::test]
    async fn preference_does_not_block_the_only_ready_input() {
        let (_shutdown, mut stop) = broadcast::channel(1);
        let (sender, mut inbox) = mpsc::channel(1);
        let mut timer = timer().await;
        let mut scheduler = Scheduler::default();
        for _ in 0..2 {
            overdue(&mut timer).await;
            assert_eq!(
                scheduler.next(&mut stop, &mut inbox, &mut timer).await,
                Ok(None)
            );
        }
        timer.reset_after(Duration::from_secs(60));
        for packet in 0..2 {
            sender.send(packet).await.unwrap();
            let next = tokio::time::timeout(
                Duration::from_secs(1),
                scheduler.next(&mut stop, &mut inbox, &mut timer),
            )
            .await;
            assert_eq!(next.unwrap(), Ok(Some(packet)));
        }
    }

    #[tokio::test]
    async fn shutdown_wins_in_both_turns() {
        for packet_turn in [false, true] {
            let (shutdown, mut stop) = broadcast::channel(1);
            let (sender, mut inbox) = mpsc::channel(1);
            sender.send(1).await.unwrap();
            shutdown.send(()).unwrap();
            let mut timer = timer().await;
            let mut scheduler = Scheduler { packet_turn };
            assert_eq!(
                scheduler.next(&mut stop, &mut inbox, &mut timer).await,
                Err(())
            );
            assert_eq!(inbox.try_recv().unwrap(), 1);
        }
    }

    #[tokio::test]
    async fn closed_inbox_is_observed_after_at_most_one_poll() {
        let (_shutdown, mut stop) = broadcast::channel(1);
        let (sender, mut inbox) = mpsc::channel::<u8>(1);
        drop(sender);
        let mut timer = timer().await;
        let mut scheduler = Scheduler::default();
        assert_eq!(
            scheduler.next(&mut stop, &mut inbox, &mut timer).await,
            Ok(None)
        );
        overdue(&mut timer).await;
        assert_eq!(
            scheduler.next(&mut stop, &mut inbox, &mut timer).await,
            Err(())
        );
    }
}
