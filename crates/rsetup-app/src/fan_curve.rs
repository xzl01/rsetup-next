use anyhow::Result;
use rsetup_core::{Controller, FanCurveConfig, FanCurvePoint, FanCurveRequest};
use std::time::Duration;

pub(crate) fn fan_curve_request(
    zone_id: String,
    cooling_device_id: String,
    points: Vec<FanCurvePoint>,
    poll_interval_ms: u32,
    hysteresis_c: f32,
) -> FanCurveRequest {
    FanCurveRequest {
        enabled: true,
        config: Some(FanCurveConfig {
            zone_id,
            cooling_device_id,
            poll_interval_ms,
            hysteresis_c,
            points,
        }),
    }
}

pub(crate) async fn run_fan_curve_daemon(controller: &Controller) -> Result<()> {
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        let tick = match controller.fan_curve_tick() {
            Ok(tick) => tick,
            Err(error) => {
                force_fan_curve_failsafe(controller, "fatal controller error");
                return Err(error.into());
            }
        };
        if tick.failsafe {
            tracing::warn!(
                cooling_state = tick.cooling_state,
                "fan curve sensor fail-safe forced maximum cooling"
            );
        } else {
            tracing::debug!(
                temperature_c = tick.temperature_c,
                speed_percent = tick.speed_percent,
                cooling_state = tick.cooling_state,
                "fan curve step applied"
            );
        }
        #[cfg(unix)]
        tokio::select! {
            result = &mut interrupt => {
                result?;
                break;
            }
            _ = terminate.recv() => break,
            () = tokio::time::sleep(Duration::from_millis(u64::from(tick.poll_interval_ms))) => {}
        }
        #[cfg(not(unix))]
        tokio::select! {
            result = &mut interrupt => {
                result?;
                break;
            }
            () = tokio::time::sleep(Duration::from_millis(u64::from(tick.poll_interval_ms))) => {}
        }
    }
    force_fan_curve_failsafe(controller, "service shutdown");
    Ok(())
}

pub(crate) fn force_fan_curve_failsafe(controller: &Controller, reason: &str) {
    match controller.fan_curve_shutdown_failsafe() {
        Ok(tick) => tracing::warn!(
            cooling_state = tick.cooling_state,
            reason,
            "fan curve fail-safe forced maximum cooling"
        ),
        Err(error) => tracing::error!(%error, reason, "unable to force maximum cooling"),
    }
}
