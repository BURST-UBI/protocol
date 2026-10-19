//! Windows SCM lifecycle. Stop requests use the node's ordinary drain and flush.

use std::{ffi::OsString, time::Duration};

use clap::Parser;
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle},
    service_dispatcher,
};

const NAME: &str = "BurstNode";

define_windows_service!(entry, service_main);

pub(super) fn dispatch() -> anyhow::Result<()> {
    service_dispatcher::start(NAME, entry)?;
    Ok(())
}

fn report(
    handle: &ServiceStatusHandle,
    state: ServiceState,
    checkpoint: u32,
    failed: bool,
) -> windows_service::Result<()> {
    let pending = matches!(
        state,
        ServiceState::StartPending | ServiceState::StopPending
    );
    handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: if failed {
            ServiceExitCode::ServiceSpecific(1)
        } else {
            ServiceExitCode::Win32(0)
        },
        checkpoint: if pending { checkpoint } else { 0 },
        wait_hint: if pending {
            Duration::from_secs(30)
        } else {
            Duration::ZERO
        },
        process_id: None,
    })
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(error) = serve() {
        tracing::error!(%error, "Windows service failed");
    }
}

fn serve() -> anyhow::Result<()> {
    let (stop_sender, stop) = tokio::sync::watch::channel(false);
    let stop_on_error = stop_sender.clone();
    let handle = service_control_handler::register(NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_sender.send(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    report(&handle, ServiceState::StartPending, 1, false)?;
    let result = (|| {
        let cli = super::Cli::try_parse()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let (ready_sender, mut ready) = tokio::sync::oneshot::channel();
            let node = super::run(cli, Some(stop.clone()), Some(ready_sender));
            tokio::pin!(node);
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut running = false;
            let mut ready_open = true;
            let mut checkpoint = 1u32;
            loop {
                tokio::select! {
                    result = &mut node => break result,
                    result = &mut ready, if ready_open => {
                        ready_open = false;
                        running = result.is_ok();
                        if running && !*stop.borrow() {
                            if let Err(error) = report(&handle, ServiceState::Running, 0, false) {
                                let _ = stop_on_error.send(true);
                                let _ = node.await;
                                break Err(error.into());
                            }
                        }
                    }
                    _ = interval.tick() => {
                        let state = if *stop.borrow() { ServiceState::StopPending }
                            else if running { ServiceState::Running } else { ServiceState::StartPending };
                        checkpoint = checkpoint.saturating_add(1);
                        if let Err(error) = report(&handle, state, checkpoint, false) {
                            let _ = stop_on_error.send(true);
                            let _ = node.await;
                            break Err(error.into());
                        }
                    }
                }
            }
        })
    })();
    report(&handle, ServiceState::Stopped, 0, result.is_err())?;
    result
}
