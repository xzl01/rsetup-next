use rsetup_core::{SystemInstallApplyResult, SystemInstallError, SystemInstallRequest};
use std::path::Path;

use crate::i18n::Locale;

/// Path of the root-only helper and the authorizer that starts it. The helper
/// accepts fixed verbs only. Shared so a surface test can prove the delegated
/// route without ever starting it.
pub(crate) const PRIVILEGED_HELPER: &str = rsetup_core::privileged_helper_path();
const PKEXEC: &str = rsetup_core::pkexec_path();

/// Stable codes for a system-install failure. The vocabulary lives on the error
/// itself so the helper binary, which does not share this crate root, reports the
/// same codes.
pub(crate) fn system_install_error_code(error: &SystemInstallError) -> &'static str {
    error.code()
}

pub(crate) fn effective_uid() -> Option<u32> {
    rsetup_core::effective_uid()
}

/// Whether a real write has to be delegated to the fixed-verb helper.
///
/// The demo provider writes to nothing, and root opens the device itself;
/// anything else is refused by the kernel when it opens the target, so routing
/// it through `pkexec` is what turns a `PermissionDenied` into an administrator
/// prompt. The CLI has always decided this way; the TUI and the HTTP surface now
/// share the rule instead of each writing its own copy of it.
pub(crate) fn install_needs_privilege(
    live: bool,
    synthetic: bool,
    effective_uid: Option<u32>,
) -> bool {
    rsetup_core::install_needs_privilege(live, synthetic, effective_uid)
}

/// The outcome of a privileged install apply.
///
/// A cancelled administrator prompt is a decision, not a fault in the write, so
/// it is kept apart from the core's errors instead of being flattened into an
/// I/O failure. Every surface already has one stable code for it —
/// `authorization_canceled`, the same one the other privileged operations use —
/// and this is what lets an install report it too.
pub(crate) enum InstallApplyError {
    Core(SystemInstallError),
    AuthorizationCanceled,
    /// A refusal the privileged helper classified, carried across the process
    /// boundary. The helper runs the same core, so its code is the same
    /// vocabulary; without this every distinct refusal reached the caller as one
    /// generic error, because only the prose survived the boundary.
    Helper {
        code: String,
        message: String,
    },
}

impl From<SystemInstallError> for InstallApplyError {
    fn from(error: SystemInstallError) -> Self {
        Self::Core(error)
    }
}

impl InstallApplyError {
    /// The stable code a script reads, whichever surface it came through.
    ///
    /// Borrowed rather than `'static`: a code that crossed the helper boundary is
    /// owned data, not a literal.
    pub(crate) fn code(&self) -> &str {
        match self {
            Self::Core(error) => system_install_error_code(error),
            Self::AuthorizationCanceled => "authorization_canceled",
            // The helper already classified the failure; do not re-derive it from
            // the prose.
            Self::Helper { code, .. } => code.as_str(),
        }
    }

    /// Human copy for the CLI and the TUI. The HTTP and IPC envelopes carry
    /// `code`, and the browser page replaces this text with its own translation
    /// of that code, so the wording here is the fallback for a reader without a
    /// catalogue — which is why it is written out rather than keyed.
    pub(crate) fn message(&self, locale: Locale) -> String {
        match self {
            Self::Core(error) => locale.install_error(error),
            Self::AuthorizationCanceled => if locale.is_zh() {
                "已取消管理员授权。"
            } else {
                "Administrator authorization canceled."
            }
            .into(),
            // The helper's own wording already names the device, the image and the
            // expected acknowledgement, so it is a better explanation than a
            // reconstruction from the code alone.
            Self::Helper { message, .. } => message.clone(),
        }
    }
}

/// The root-only helper's fixed verb: no shell, no arbitrary arguments, and the
/// helper re-validates the image and re-judges the typed name with the core.
///
/// Shared by every surface that can write: the CLI, the TUI and the HTTP server
/// all preview in their own process and then hand the plan token to the helper,
/// which treats it as the advisory preview it is and issues its own token.
/// Reads the helper's error envelope: `{"error":{"code","message"}}`.
///
/// Returns `None` when stdout is not that shape, so a helper built before the
/// envelope still reports through its exit status and stderr.
fn helper_error_envelope(stdout: &[u8]) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    let error = value.get("error")?;
    let code = error.get("code")?.as_str()?.to_owned();
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some((code, message))
}

pub(crate) fn run_privileged_system_install(
    request: &SystemInstallRequest,
    plan_token: &str,
    acknowledgement: &str,
) -> Result<SystemInstallApplyResult, InstallApplyError> {
    if !Path::new(PRIVILEGED_HELPER).is_file() {
        return Err(SystemInstallError::RootRequired.into());
    }
    let Some(image_path) = request.image_path.as_deref() else {
        return Err(SystemInstallError::PlanRequired.into());
    };
    let output = std::process::Command::new(PKEXEC)
        .args([
            PRIVILEGED_HELPER,
            "system-install-apply",
            &request.target_id,
            image_path,
            plan_token,
            acknowledgement,
            "--confirmed",
        ])
        .output()
        .map_err(|error| {
            InstallApplyError::Core(SystemInstallError::Io(format!(
                "unable to start the privileged helper: {error}"
            )))
        })?;
    if output.status.code() == Some(126) {
        // polkit's "not authorized": the administrator dismissed the prompt.
        return Err(InstallApplyError::AuthorizationCanceled);
    }
    if !output.status.success() {
        if let Some((code, message)) = helper_error_envelope(&output.stdout) {
            return Err(InstallApplyError::Helper { code, message });
        }
        // A helper built before the envelope, or one that died before printing
        // it: fall back to its exit status and stderr.
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(SystemInstallError::Io(format!(
            "the privileged helper failed: {}",
            detail.trim()
        ))
        .into());
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        InstallApplyError::Core(SystemInstallError::Io(format!(
            "invalid helper response: {error}"
        )))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_error_codes_are_stable_across_surfaces() {
        let cases: Vec<(SystemInstallError, &str)> = vec![
            (
                SystemInstallError::Unsupported("x".into()),
                "install_unsupported",
            ),
            (
                SystemInstallError::InvalidInput("x".into()),
                "invalid_install_request",
            ),
            (
                SystemInstallError::ConfirmationRequired,
                "confirmation_required",
            ),
            (
                SystemInstallError::AcknowledgementMismatch {
                    expected: "sda".into(),
                    target: "/dev/sda".into(),
                    provided: "true".into(),
                },
                "acknowledgement_mismatch",
            ),
            (SystemInstallError::PlanRequired, "plan_required"),
            (SystemInstallError::StalePlan, "stale_plan"),
            (SystemInstallError::RootRequired, "root_required"),
            (SystemInstallError::Conflict("x".into()), "install_conflict"),
            (
                SystemInstallError::RunningSystem("/dev/mmcblk1".into()),
                "writes_running_system",
            ),
            (SystemInstallError::Io("x".into()), "internal_error"),
        ];
        for (error, code) in cases {
            assert_eq!(system_install_error_code(&error), code);
            assert!(!Locale::En.install_error(&error).is_empty());
            assert!(!Locale::ZhCn.install_error(&error).is_empty());
        }
    }

    #[test]
    fn privileged_install_without_the_helper_requires_root() {
        if Path::new(PRIVILEGED_HELPER).is_file() {
            // A host that ships the helper cannot assert its absence; the real
            // path is exercised on the board.
            return;
        }
        let request = SystemInstallRequest {
            target_id: "sda".into(),
            image_path: Some("/tmp/radxa.img".into()),
            image_id: None,
        };
        assert!(matches!(
            run_privileged_system_install(&request, "install-plan-0", "sda"),
            Err(InstallApplyError::Core(SystemInstallError::RootRequired))
        ));
    }

    /// The routing rule itself, without root, without `pkexec` and without a
    /// device. The surfaces all read this one function, so this table is the
    /// guard against one of them silently keeping an in-process write.
    /// The helper's envelope is what keeps a distinct refusal distinct. A helper
    /// that predates it, or that died before printing one, must still degrade to
    /// the old behaviour rather than being misread.
    #[test]
    fn the_helper_error_envelope_is_read_only_when_it_is_one() {
        let envelope = br#"{"error":{"code":"stale_plan","message":"the target changed"}}"#;
        assert_eq!(
            helper_error_envelope(envelope),
            Some(("stale_plan".to_owned(), "the target changed".to_owned()))
        );
        // A success payload is not an envelope.
        assert_eq!(helper_error_envelope(br#"{"bytesWritten":1}"#), None);
        // Prose from a helper that predates the envelope, or a truncated write.
        assert_eq!(
            helper_error_envelope(b"Error: something went wrong\n"),
            None
        );
        assert_eq!(
            helper_error_envelope(br#"{"error":{"message":"no code"}}"#),
            None
        );
    }

    #[test]
    fn install_privilege_routing_follows_policy_provider_and_uid() {
        let cases = [
            // Ordinary user, live policy, real device: the reported defect. It
            // must be delegated, or opening the device just fails as EACCES.
            (true, false, Some(1000), true),
            // Root already holds the privilege the helper would grant.
            (true, false, Some(0), false),
            // The demo provider writes to nothing, so there is nothing to authorize.
            (true, true, Some(1000), false),
            // A dry run never reaches a device.
            (false, false, Some(1000), false),
            // An unreadable euid is not evidence of root.
            (true, false, None, true),
        ];
        for (live, synthetic, uid, expected) in cases {
            assert_eq!(
                install_needs_privilege(live, synthetic, uid),
                expected,
                "live={live} synthetic={synthetic} uid={uid:?}"
            );
        }
    }

    /// A dismissed administrator prompt is a decision, so it must not be
    /// flattened into the code a hard I/O failure reports.
    #[test]
    fn cancelled_authorization_keeps_its_own_code() {
        let cancelled = InstallApplyError::AuthorizationCanceled;
        assert_eq!(cancelled.code(), "authorization_canceled");
        assert!(!cancelled.message(Locale::En).is_empty());
        assert!(!cancelled.message(Locale::ZhCn).is_empty());
        assert_ne!(
            cancelled.code(),
            InstallApplyError::Core(SystemInstallError::Io("x".into())).code()
        );
    }
}
