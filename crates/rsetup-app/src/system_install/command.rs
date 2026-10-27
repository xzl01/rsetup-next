use anyhow::{Result, anyhow};
use rsetup_core::{
    SystemInstallApplyResult, SystemInstallError, SystemInstallManager, SystemInstallPlan,
    SystemInstallRequest,
};
use std::io::{BufRead, IsTerminal, Write};

use crate::cli::SystemCommands;
use crate::i18n::Locale;
use crate::system_install::privileged::{
    InstallApplyError, effective_uid, install_needs_privilege, run_privileged_system_install,
};
use crate::system_install::render::{
    write_install_plan, write_install_result, write_install_targets,
};
use crate::system_install::{InstallOptions, system_install_manager, system_install_request};

/// Where the write happens: in this process, or through the root-only helper.
enum InstallExecer<'a> {
    InProcess(&'a SystemInstallManager),
    Privileged,
}

pub(crate) fn run_system_command(
    options: InstallOptions,
    locale: Locale,
    command: SystemCommands,
) -> Result<()> {
    match command {
        SystemCommands::Targets { json } => {
            let manager = system_install_manager(options);
            let targets = manager
                .targets()
                .map_err(|error| install_failure(error.into(), locale, json))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&targets)?);
            } else {
                write_install_targets(&targets, locale, &mut std::io::stdout())?;
            }
            Ok(())
        }
        SystemCommands::Plan {
            target,
            image,
            json,
        } => {
            let manager = system_install_manager(options);
            let request = system_install_request(&target, &image);
            let plan = manager
                .plan(&request)
                .map_err(|error| install_failure(error.into(), locale, json))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                write_install_plan(&plan, locale, &mut std::io::stdout())?;
            }
            Ok(())
        }
        SystemCommands::Install {
            target,
            image,
            acknowledge,
            json,
        } => {
            let manager = system_install_manager(options);
            let request = system_install_request(&target, &image);
            // A live write that needs root goes through the fixed-verb helper,
            // exactly like the other privileged operations.
            let execer =
                if install_needs_privilege(options.live, options.synthetic, effective_uid()) {
                    InstallExecer::Privileged
                } else {
                    InstallExecer::InProcess(&manager)
                };
            let stdin = std::io::stdin();
            if acknowledge.is_none() && !std::io::stdin().is_terminal() {
                eprintln!("{}", locale.text("install_not_a_terminal"));
            }
            let mut input = stdin.lock();
            // In JSON mode every human-readable byte goes to stderr, so stdout
            // carries exactly one JSON document that a caller can parse. The
            // preview and the typed-confirmation prompt are a conversation with a
            // person, not part of the machine-readable result.
            let mut out: Box<dyn std::io::Write> = if json {
                Box::new(std::io::stderr())
            } else {
                Box::new(std::io::stdout())
            };
            let result = run_system_install(
                &manager,
                &execer,
                &request,
                acknowledge,
                locale,
                !options.live,
                &mut input,
                &mut *out,
            )
            .map_err(|error| install_failure(error, locale, json))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                write_install_result(&result, locale, &mut *out)?;
            }
            Ok(())
        }
    }
}

/// Plans, shows every irreversible consequence, takes the typed device name and
/// applies it. The typed value is judged once, inside the core, so this CLI can
/// never drift from the other surfaces.
#[allow(clippy::too_many_arguments)]
fn run_system_install(
    manager: &SystemInstallManager,
    execer: &InstallExecer<'_>,
    request: &SystemInstallRequest,
    acknowledgement: Option<String>,
    locale: Locale,
    dry_run: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<SystemInstallApplyResult, InstallApplyError> {
    let plan = manager.plan(request)?;
    write_install_plan(&plan, locale, out).map_err(io_failure)?;
    if dry_run {
        writeln!(out, "{}", locale.text("install_dry_run_notice")).map_err(io_failure)?;
    }
    let acknowledgement = match acknowledgement {
        Some(value) => value,
        None => prompt_install_acknowledgement(&plan, locale, input, out).map_err(io_failure)?,
    };
    match execer {
        InstallExecer::InProcess(manager) => manager
            .apply_live(request, &plan.plan_token, &acknowledgement)
            .map_err(InstallApplyError::Core),
        InstallExecer::Privileged => {
            run_privileged_system_install(request, &plan.plan_token, &acknowledgement)
        }
    }
}

fn io_failure(error: std::io::Error) -> SystemInstallError {
    SystemInstallError::Io(format!("unable to read the typed confirmation: {error}"))
}

/// Asks for the device name and returns exactly what the user typed; the core
/// decides whether it is acceptable.
fn prompt_install_acknowledgement(
    plan: &SystemInstallPlan,
    locale: Locale,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> std::io::Result<String> {
    writeln!(
        out,
        "{}: {}",
        locale.text("install_confirm_prompt"),
        plan.target.id
    )?;
    writeln!(out, "{}", locale.text("install_confirm_hint"))?;
    write!(out, "> ")?;
    out.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

/// JSON mode prints the machine-readable envelope on stdout; the returned error
/// always carries the stable code for the human-readable path.
fn install_failure(error: InstallApplyError, locale: Locale, json: bool) -> anyhow::Error {
    let code = error.code();
    let message = error.message(locale);
    if json {
        println!(
            "{}",
            serde_json::json!({"error": {"code": code, "message": message}})
        );
    }
    anyhow!("{code}: {message}")
}

#[cfg(test)]
pub(in crate::system_install) mod tests {
    use super::*;
    use crate::system_install::fixtures::{
        demo_install_manager, install_request_for, install_scratch, mbr_image,
    };
    use std::fs;
    use std::path::Path;

    /// Runs the CLI install flow with an injected terminal.
    pub(in crate::system_install) fn run_install(
        manager: &SystemInstallManager,
        target: &str,
        image: &Path,
        acknowledgement: Option<&str>,
        typed: &str,
        dry_run: bool,
    ) -> (Result<SystemInstallApplyResult, SystemInstallError>, String) {
        let request = install_request_for(target, image);
        let execer = InstallExecer::InProcess(manager);
        let mut input = std::io::Cursor::new(typed.as_bytes().to_vec());
        let mut out = Vec::new();
        let result = run_system_install(
            manager,
            &execer,
            &request,
            acknowledgement.map(str::to_owned),
            Locale::En,
            dry_run,
            &mut input,
            &mut out,
        )
        // Every test drives the in-process execer, which cannot cancel, so the
        // plain core error is what these cases are about.
        .map_err(|error| match error {
            InstallApplyError::Core(error) => error,
            InstallApplyError::AuthorizationCanceled => {
                SystemInstallError::Io("authorization canceled".into())
            }
            InstallApplyError::Helper { message, .. } => SystemInstallError::Io(message),
        });
        (result, String::from_utf8(out).expect("utf-8 output"))
    }

    #[test]
    fn install_shows_consequences_before_prompting() {
        let scratch = install_scratch("plan-first");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);
        let (result, output) = run_install(&manager, "sda", &image, None, "sda\n", true);
        result.expect("the demo install succeeds");
        let plan_at = output
            .find("System image write preview")
            .expect("plan shown");
        let prompt_at = output
            .find("Type the target device name")
            .expect("prompt shown");
        assert!(plan_at < prompt_at, "the consequences come first: {output}");
        assert!(output.contains("/dev/sda"), "target identity: {output}");
        assert!(output.contains("RADXA UFS 128GB"), "target model: {output}");
        assert!(output.contains("Existing partitions destroyed"), "{output}");
        assert!(
            output.contains("sda1") || output.contains("sda2"),
            "{output}"
        );
        assert!(output.contains("SHA-256"), "image identity: {output}");
        assert!(
            output.contains("Writing replaces every partition"),
            "{output}"
        );
        assert!(output.contains("cannot be rolled back"), "{output}");
        assert!(output.contains("Dry run: nothing is written"), "{output}");
        assert!(
            output.chars().filter(|c| *c == '\n').count() > 12,
            "the checklist is printed in full"
        );
    }

    #[test]
    fn install_requires_the_typed_device_name() {
        let scratch = install_scratch("typed-name");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);

        // Empty input is not a confirmation.
        let (result, output) = run_install(&manager, "sda", &image, None, "\n", true);
        assert!(
            matches!(result, Err(SystemInstallError::ConfirmationRequired)),
            "{result:?}"
        );
        assert!(output.contains("Type the target device name"), "{output}");

        // A boolean-looking value cannot pass; the name must match exactly.
        for typed in ["true", "yes", "1", "sda1", "/dev/sda", "SDA"] {
            let (result, _) = run_install(&manager, "sda", &image, None, typed, true);
            match result {
                Err(SystemInstallError::AcknowledgementMismatch {
                    expected, provided, ..
                }) => {
                    assert_eq!(expected, "sda");
                    assert_eq!(provided, typed);
                }
                other => panic!("{typed} must be rejected: {other:?}"),
            }
        }

        // The exact device name is accepted, and the write happens through the
        // same core call the other surfaces will use.
        let (result, _) = run_install(&manager, "sda", &image, None, "sda\n", true);
        let result = result.expect("the typed name is accepted");
        assert_eq!(result.plan.target.id, "sda");
        assert_eq!(result.bytes_written, mbr_image().len() as u64);
        assert!(result.run.synthetic, "the demo provider is labelled");

        // --acknowledge carries the same string, and is still judged by the core.
        let (result, _) = run_install(&manager, "sda", &image, Some("sda1"), "", true);
        assert!(matches!(
            result,
            Err(SystemInstallError::AcknowledgementMismatch { .. })
        ));
        let (result, _) = run_install(&manager, "sda", &image, Some("sda"), "", false);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn install_refuses_the_device_that_hosts_the_running_system() {
        let scratch = install_scratch("running-system");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);
        // The demo SD card carries `/` and `/boot`, and no typed name can change
        // that.
        let (result, output) = run_install(&manager, "mmcblk1", &image, Some("mmcblk1"), "", true);
        assert!(
            matches!(result, Err(SystemInstallError::RunningSystem(ref path)) if path == "/dev/mmcblk1"),
            "{result:?}"
        );
        assert!(
            !output.contains("Type the target device name"),
            "the refusal happens before any confirmation: {output}"
        );
    }

    #[test]
    fn install_rejects_an_untrusted_image_path() {
        let scratch = install_scratch("image-path");
        let real = scratch.join("real.img");
        fs::write(&real, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);

        // Relative paths never reach the privileged side.
        let (result, _) = run_install(
            &manager,
            "sda",
            Path::new("system.img"),
            Some("sda"),
            "",
            true,
        );
        assert!(
            matches!(result, Err(SystemInstallError::InvalidInput(ref detail)) if detail.contains("absolute")),
            "{result:?}"
        );

        // A symlink is not a regular file.
        let link = scratch.join("link.img");
        std::os::unix::fs::symlink(&real, &link).expect("create symlink");
        let (result, _) = run_install(&manager, "sda", &link, Some("sda"), "", true);
        assert!(
            matches!(result, Err(SystemInstallError::InvalidInput(ref detail)) if detail.contains("regular file")),
            "{result:?}"
        );

        // A missing file is reported before any write.
        let (result, _) = run_install(
            &manager,
            "sda",
            &scratch.join("absent.img"),
            Some("sda"),
            "",
            true,
        );
        assert!(
            matches!(result, Err(SystemInstallError::InvalidInput(_))),
            "{result:?}"
        );

        // An image that is not a disk image is refused.
        let empty = scratch.join("empty.img");
        fs::write(&empty, b"not an image").expect("write file");
        let (result, _) = run_install(&manager, "sda", &empty, Some("sda"), "", true);
        assert!(
            matches!(result, Err(SystemInstallError::InvalidInput(_))),
            "{result:?}"
        );
    }
}
