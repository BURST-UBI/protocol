//! Read-only adapter for ntpd-rs' authenticated packet validation. No clock
//! discipline or statistical estimate is used as a signing-time bound.

use ntp_proto::{
    Measurement, NtpClock, NtpDuration, NtpLeapIndicator, NtpTimestamp, ObservableSourceTimedata,
    PollInterval, SourceConfig, SourceController, StateUpdate, SynchronizationConfig,
    TimeSyncController,
};
use std::{
    io,
    marker::PhantomData,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) type Sample = Measurement<NtpDuration>;

#[derive(Clone, Default)]
pub(super) struct ReadOnlyClock(pub Arc<Mutex<Option<Sample>>>);

pub(super) fn timestamp(time: SystemTime) -> io::Result<NtpTimestamp> {
    let unix = time.duration_since(UNIX_EPOCH).map_err(io::Error::other)?;
    Ok(NtpTimestamp::from_seconds_nanos_since_ntp_era(
        unix.as_secs().wrapping_add(2_208_988_800) as u32,
        unix.subsec_nanos(),
    ))
}

fn read_only<T>() -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "BURST never disciplines the host clock",
    ))
}

impl NtpClock for ReadOnlyClock {
    type Error = io::Error;
    fn now(&self) -> io::Result<NtpTimestamp> {
        timestamp(SystemTime::now())
    }
    fn set_frequency(&self, _: f64) -> io::Result<NtpTimestamp> {
        read_only()
    }
    fn get_frequency(&self) -> io::Result<f64> {
        read_only()
    }
    fn step_clock(&self, _: NtpDuration) -> io::Result<NtpTimestamp> {
        read_only()
    }
    fn disable_ntp_algorithm(&self) -> io::Result<()> {
        read_only()
    }
    fn error_estimate_update(&self, _: NtpDuration, _: NtpDuration) -> io::Result<()> {
        read_only()
    }
    fn status_update(&self, _: NtpLeapIndicator) -> io::Result<()> {
        read_only()
    }
}

pub(super) struct CaptureController(ReadOnlyClock);
pub(super) struct CaptureSource<D> {
    clock: ReadOnlyClock,
    delay: PhantomData<D>,
}

impl TimeSyncController for CaptureController {
    type Clock = ReadOnlyClock;
    type SourceId = usize;
    type AlgorithmConfig = ();
    type ControllerMessage = ();
    type SourceMessage = ();
    type NtpSourceController = CaptureSource<NtpDuration>;
    type OneWaySourceController = CaptureSource<()>;

    fn new(clock: ReadOnlyClock, _: SynchronizationConfig, _: ()) -> io::Result<Self> {
        Ok(Self(clock))
    }
    fn take_control(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn add_source(&mut self, _: usize, _: SourceConfig) -> Self::NtpSourceController {
        CaptureSource {
            clock: self.0.clone(),
            delay: PhantomData,
        }
    }
    fn add_one_way_source(
        &mut self,
        _: usize,
        _: SourceConfig,
        _: f64,
        _: f64,
        _: Option<f64>,
    ) -> Self::OneWaySourceController {
        CaptureSource {
            clock: self.0.clone(),
            delay: PhantomData,
        }
    }
    fn remove_source(&mut self, _: usize) {}
    fn source_update(&mut self, _: usize, _: bool) {}
    fn source_message(&mut self, _: usize, _: ()) -> StateUpdate<usize, ()> {
        StateUpdate::default()
    }
    fn time_update(&mut self) -> StateUpdate<usize, ()> {
        StateUpdate::default()
    }
}

fn unobserved() -> ObservableSourceTimedata {
    ObservableSourceTimedata {
        uncertainty: NtpDuration::MAX,
        ..Default::default()
    }
}

impl SourceController for CaptureSource<NtpDuration> {
    type ControllerMessage = ();
    type SourceMessage = ();
    type MeasurementDelay = NtpDuration;
    fn handle_message(&mut self, _: ()) {}
    fn handle_measurement(&mut self, sample: Sample) -> Option<()> {
        if let Ok(mut slot) = self.clock.0.lock() {
            *slot = Some(sample);
        }
        None
    }
    fn desired_poll_interval(&self) -> PollInterval {
        PollInterval::from_byte(6)
    }
    fn observe(&self) -> ObservableSourceTimedata {
        unobserved()
    }
}

// Required by the upstream controller trait. One-way/GPS input is deliberately
// not admitted into this NTS-only measurement path.
impl SourceController for CaptureSource<()> {
    type ControllerMessage = ();
    type SourceMessage = ();
    type MeasurementDelay = ();
    fn handle_message(&mut self, _: ()) {}
    fn handle_measurement(&mut self, _: Measurement<()>) -> Option<()> {
        None
    }
    fn desired_poll_interval(&self) -> PollInterval {
        PollInterval::NEVER
    }
    fn observe(&self) -> ObservableSourceTimedata {
        unobserved()
    }
}
