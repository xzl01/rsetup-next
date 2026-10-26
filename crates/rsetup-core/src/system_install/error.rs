//! The single error type of this module and the stable code vocabulary every
//! surface reports.

use thiserror::Error;

impl SystemInstallError {
    /// Stable code for this failure. This is the vocabulary every surface
    /// reports — the CLI, the HTTP envelope, the Tauri command error and the
    /// privileged helper's error envelope — so a script reads one code whichever
    /// door it came through.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported(_) => "install_unsupported",
            Self::InvalidInput(_) => "invalid_install_request",
            Self::ConfirmationRequired => "confirmation_required",
            Self::AcknowledgementMismatch { .. } => "acknowledgement_mismatch",
            Self::PlanRequired => "plan_required",
            Self::StalePlan => "stale_plan",
            Self::RootRequired => "root_required",
            Self::Conflict(_) => "install_conflict",
            Self::RunningSystem(_) => "writes_running_system",
            Self::Io(_) => "internal_error",
        }
    }
}

#[derive(Debug, Error)]
pub enum SystemInstallError {
    #[error("system image installation is unavailable: {0}")]
    Unsupported(String),
    #[error("invalid system install request: {0}")]
    InvalidInput(String),
    #[error("confirmation is required before writing a system image")]
    ConfirmationRequired,
    #[error("type \"{expected}\" to write {target}; received \"{provided}\"")]
    AcknowledgementMismatch {
        expected: String,
        target: String,
        provided: String,
    },
    #[error("preview the system image write before applying it")]
    PlanRequired,
    #[error("the target or the image changed after preview; create a fresh plan")]
    StalePlan,
    #[error("writing a system image requires root privileges")]
    RootRequired,
    #[error("the target cannot be written: {0}")]
    Conflict(String),
    #[error("refusing to write {0}: it hosts the running root or boot filesystem")]
    RunningSystem(String),
    #[error("unable to write the system image: {0}")]
    Io(String),
}
