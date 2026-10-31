//! Opt-in wall-time attribution, not exclusive CPU time or a throughput proof.
//! Fixed storage and one summary per stage on reader exit bound diagnostic cost.
use std::{path::Path, time::Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    ReadFrame,
    Decode,
    Permit,
    Dedup,
    Liveness,
    Signature,
    Authorize,
    Online,
    Dispatch,
    DispatchWeight,
    DispatchWait,
    DispatchRoute,
    Relay,
    History,
    Block,
    ConfirmRequest,
    Keepalive,
    Bootstrap,
    Other,
}
const LABELS: [&str; 19] = [
    "read_frame",
    "decode",
    "permit",
    "dedup",
    "liveness",
    "signature",
    "authorize",
    "online",
    "dispatch",
    "dispatch_weight",
    "dispatch_wait",
    "dispatch_route",
    "relay",
    "history",
    "block",
    "confirm_request",
    "keepalive",
    "bootstrap",
    "other",
];
#[derive(Clone, Copy, Default)]
struct Sample {
    segments: u64,
    total_us: u128,
    max_us: u128,
    over_1ms: u64,
    over_10ms: u64,
    over_100ms: u64,
}
impl Sample {
    fn record(&mut self, micros: u128) {
        self.segments = self.segments.saturating_add(1);
        self.total_us = self.total_us.saturating_add(micros);
        self.max_us = self.max_us.max(micros);
        self.over_1ms = self.over_1ms.saturating_add(u64::from(micros >= 1_000));
        self.over_10ms = self.over_10ms.saturating_add(u64::from(micros >= 10_000));
        self.over_100ms = self.over_100ms.saturating_add(u64::from(micros >= 100_000));
    }
}
struct Enabled<'a> {
    peer: &'a str,
    receiver: &'a Path,
    stage: Stage,
    since: Instant,
    samples: [Sample; 19],
}
// Disabled readers retain only a null pointer, with no clock calls or allocation.
pub(super) struct ReceiveProfile<'a>(Option<Box<Enabled<'a>>>);
impl<'a> ReceiveProfile<'a> {
    pub(super) fn new(enabled: bool, peer: &'a str, receiver: &'a Path) -> Self {
        Self(enabled.then(|| {
            Box::new(Enabled {
                peer,
                receiver,
                stage: Stage::ReadFrame,
                since: Instant::now(),
                samples: [Sample::default(); 19],
            })
        }))
    }
    pub(super) fn stage(&mut self, stage: Stage) {
        let Some(profile) = self.0.as_mut() else {
            return;
        };
        if profile.stage == stage {
            return;
        }
        let now = Instant::now();
        profile.samples[profile.stage as usize]
            .record(now.duration_since(profile.since).as_micros());
        profile.stage = stage;
        profile.since = now;
    }
}
impl Drop for ReceiveProfile<'_> {
    fn drop(&mut self) {
        let Some(profile) = self.0.as_mut() else {
            return;
        };
        profile.samples[profile.stage as usize].record(profile.since.elapsed().as_micros());
        for (stage, sample) in LABELS.iter().zip(&profile.samples) {
            if sample.segments == 0 {
                continue;
            }
            tracing::debug!(receiver = ?profile.receiver, peer = profile.peer, stage,
                segments = sample.segments, total_us = %sample.total_us, max_us = %sample.max_us,
                over_1ms = sample.over_1ms, over_10ms = sample.over_10ms, over_100ms = sample.over_100ms,
                "receive stage summary");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage_samples_preserve_thresholds_and_large_totals() {
        let mut sample = Sample::default();
        for micros in [
            0,
            999,
            1_000,
            9_999,
            10_000,
            99_999,
            100_000,
            u64::MAX as u128,
        ] {
            sample.record(micros);
        }
        assert_eq!(sample.segments, 8);
        assert_eq!(sample.total_us, u64::MAX as u128 + 221_997);
        assert_eq!(sample.max_us, u64::MAX as u128);
        assert_eq!(
            (sample.over_1ms, sample.over_10ms, sample.over_100ms),
            (6, 4, 2)
        );
    }
    #[test]
    fn disabled_profile_has_no_state_and_same_stage_does_not_split_segments() {
        let mut disabled = ReceiveProfile::new(false, "peer", Path::new("store"));
        disabled.stage(Stage::Dispatch);
        assert!(disabled.0.is_none());
        let mut enabled = ReceiveProfile::new(true, "peer", Path::new("store"));
        enabled.stage(Stage::ReadFrame);
        assert_eq!(
            enabled.0.as_ref().unwrap().samples[Stage::ReadFrame as usize].segments,
            0
        );
        enabled.stage(Stage::Decode);
        enabled.stage(Stage::Decode);
        assert_eq!(
            enabled.0.as_ref().unwrap().samples[Stage::ReadFrame as usize].segments,
            1
        );
        assert_eq!(
            enabled.0.as_ref().unwrap().samples[Stage::Decode as usize].segments,
            0
        );
    }
}
