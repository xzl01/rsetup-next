use rsetup_core::{
    SystemInstallApplyResult, SystemInstallPlan, SystemInstallTarget,
    SystemInstallVerificationState,
};
use std::io::Write;

use crate::i18n::Locale;
use crate::storage_format::format_bytes;

pub(in crate::system_install) fn write_install_targets(
    targets: &[SystemInstallTarget],
    locale: Locale,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    for target in targets {
        let mut notes = Vec::new();
        if target.removable {
            notes.push(locale.text("install_removable").to_string());
        }
        if !target.mount_points.is_empty() {
            notes.push(format!(
                "{} {}",
                locale.text("install_mounted"),
                target.mount_points.join(", ")
            ));
        }
        if target.holds_running_system {
            notes.push(locale.text("install_running_system").to_string());
        }
        let suffix = if notes.is_empty() {
            String::new()
        } else {
            format!(" · {}", notes.join(" · "))
        };
        writeln!(
            out,
            "{} — {} {} · {} · {} {}{}",
            target.path,
            locale.install_device_kind(target.kind),
            target.model,
            format_bytes(target.size_bytes),
            target.partitions.len(),
            locale.text("install_destroyed_partitions"),
            suffix
        )?;
    }
    Ok(())
}

/// The full consequence list a user must see before typing the device name.
pub(in crate::system_install) fn write_install_plan(
    plan: &SystemInstallPlan,
    locale: Locale,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    writeln!(out, "{}", locale.text("install_plan_title"))?;
    writeln!(
        out,
        "{}: {} — {} ({})",
        locale.text("install_target"),
        plan.target.path,
        locale.install_device_kind(plan.target.kind),
        plan.target.model
    )?;
    writeln!(
        out,
        "{}: {}",
        locale.text("install_capacity"),
        format_bytes(plan.target.size_bytes)
    )?;
    if plan.target.removable {
        writeln!(out, "{}", locale.text("install_removable"))?;
    }
    if !plan.target.mount_points.is_empty() {
        writeln!(
            out,
            "{}: {}",
            locale.text("install_mounted"),
            plan.target.mount_points.join(", ")
        )?;
    }
    if plan.target.holds_running_system {
        writeln!(out, "{}", locale.text("install_running_system"))?;
    }
    if plan.destroys.is_empty() {
        writeln!(out, "{}", locale.text("install_no_partitions"))?;
    } else {
        writeln!(
            out,
            "{} ({}):",
            locale.text("install_destroyed_partitions"),
            plan.destroys.len()
        )?;
        for partition in &plan.destroys {
            let mount = partition
                .mount_point
                .as_deref()
                .map(|point| format!(" · {} {point}", locale.text("install_partition_mounted_at")))
                .unwrap_or_default();
            writeln!(
                out,
                "  - {} · {}{mount}",
                partition.name,
                format_bytes(partition.size_bytes)
            )?;
        }
    }
    writeln!(out, "{}: {}", locale.text("install_image"), plan.image.path)?;
    writeln!(
        out,
        "  {}: {:?} · {}: {} · {}: {}",
        locale.text("install_image_format"),
        plan.image.format,
        locale.text("install_image_size"),
        format_bytes(plan.image.size_bytes),
        locale.text("install_image_uncompressed"),
        format_bytes(plan.image.uncompressed_bytes)
    )?;
    writeln!(
        out,
        "  {}: {}",
        locale.text("install_image_sha256"),
        plan.image.sha256_prefix
    )?;
    let table = plan.image.partition_table.as_deref().unwrap_or("");
    writeln!(
        out,
        "  {}: {}",
        locale.text("install_partition_table"),
        if table.is_empty() {
            locale.text("install_partition_table_unknown").to_string()
        } else {
            table.to_uppercase()
        }
    )?;
    if plan.requires_root {
        writeln!(out, "{}", locale.text("install_requires_root"))?;
    }
    for warning in &plan.warnings {
        writeln!(out, "! {}", locale.install_warning(warning))?;
    }
    Ok(())
}

pub(in crate::system_install) fn write_install_result(
    result: &SystemInstallApplyResult,
    locale: Locale,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    writeln!(out, "{}", locale.text("install_written"))?;
    writeln!(
        out,
        "  {}: {}",
        locale.text("install_target"),
        result.plan.target.path
    )?;
    writeln!(
        out,
        "  {}: {}",
        locale.text("install_bytes_written"),
        format_bytes(result.bytes_written)
    )?;
    // "Unsupported" covers two different things: a host that cannot re-read a
    // partition table, and a compressed image whose layout was never parsed. The
    // core says which one it was, so its own wording is what gets printed rather
    // than a general label that would be wrong for one of them.
    let verification = match result.verification.state {
        SystemInstallVerificationState::Matched => {
            locale.text("install_verification_matched").to_string()
        }
        SystemInstallVerificationState::Mismatched => {
            let text = locale.text("install_verification_mismatched").to_string();
            match &result.verification.detail {
                Some(detail) => format!("{text} ({detail})"),
                None => text,
            }
        }
        SystemInstallVerificationState::Unsupported => {
            // Two different situations share this state: a host that cannot re-read
            // a partition table, and a compressed image whose layout was never
            // parsed (the table it did re-read had nothing to compare against). The
            // second one carries the table it read, and saying "this host cannot
            // re-read partition tables" there would be wrong, so it gets its own
            // wording with the core's reason.
            if result.verification.observed.is_some() {
                let text = locale
                    .text("install_verification_compressed_not_compared")
                    .to_string();
                match &result.verification.detail {
                    Some(detail) => format!("{text} ({detail})"),
                    None => text,
                }
            } else {
                match &result.verification.detail {
                    Some(detail) => detail.clone(),
                    None => locale.text("install_verification_unsupported").to_string(),
                }
            }
        }
    };
    writeln!(
        out,
        "  {}: {}",
        locale.text("install_verification"),
        verification
    )?;
    if result.run.synthetic {
        writeln!(out, "  {}", locale.text("install_synthetic_notice"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_install::command::tests::run_install;
    use crate::system_install::fixtures::{
        demo_install_manager, install_request_for, install_scratch, mbr_image,
    };
    use std::fs;

    #[test]
    fn the_cli_reports_the_core_verification_reason() {
        let scratch = install_scratch("verify-line");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(true);
        let (result, _) = run_install(&manager, "sda", &image, Some("sda"), "", false);
        let result = result.expect("the write runs");
        let mut rendered = Vec::new();
        write_install_result(&result, Locale::En, &mut rendered).expect("render the result");
        let output = String::from_utf8(rendered).expect("utf-8");
        // The demo writer refuses to re-read the table, and the CLI must print that
        // reason rather than a generic label or a mismatch.
        assert_eq!(
            result.verification.state,
            SystemInstallVerificationState::Unsupported
        );
        // The value printed is the core's own reason for this run, not the
        // general "unsupported" label, which would be wrong here (the host does
        // re-read tables; this provider does not).
        assert!(
            output.contains("does not re-read partition tables"),
            "{output}"
        );
        assert!(
            !output.contains(Locale::En.text("install_verification_unsupported")),
            "a generic label must not replace the real reason: {output}"
        );
        assert!(!output.contains("DOES NOT match"), "{output}");
    }

    /// The CLI text line separates the same two situations, and keeps preferring
    /// the core's reason: the JSON already carried `detail`, so the text now agrees
    /// with it instead of printing one label for both.
    #[test]
    fn the_cli_wording_separates_the_compressed_case_from_the_platform_one() {
        let scratch = install_scratch("cli-verification-wording");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(true);
        let (result, _) = run_install(&manager, "sda", &image, Some("sda"), "", false);
        let result = result.expect("the write runs");

        for locale in [Locale::En, Locale::ZhCn] {
            for observed in [true, false] {
                let mut shaped = result.clone();
                shaped.verification = rsetup_core::SystemInstallVerification {
                    state: SystemInstallVerificationState::Unsupported,
                    expected: Vec::new(),
                    observed: observed.then(|| {
                        vec![rsetup_core::SystemInstallPartition {
                            name: "sda1".into(),
                            number: 1,
                            start_bytes: 1024 * 1024,
                            size_bytes: 4 * 1024 * 1024,
                            mount_point: None,
                        }]
                    }),
                    detail: Some(if observed {
                        "the image is compressed and was not parsed before writing, so the re-read partition table was not compared".into()
                    } else {
                        "this host cannot re-read partition tables".into()
                    }),
                };
                let mut out = Vec::new();
                write_install_result(&shaped, locale, &mut out).expect("render the result");
                let text = String::from_utf8(out).expect("utf-8");
                if observed {
                    assert!(
                        text.contains(locale.text("install_verification_compressed_not_compared")),
                        "{locale:?} must word the compressed case: {text}"
                    );
                    assert!(
                        text.contains("the image is compressed and was not parsed"),
                        "{locale:?} must carry the core's reason: {text}"
                    );
                    assert!(
                        !text.contains(Locale::ZhCn.text("install_verification_unsupported")),
                        "{locale:?}: the platform label is wrong here: {text}"
                    );
                } else {
                    assert!(
                        text.contains("cannot re-read partition tables"),
                        "{locale:?} keeps the general reason: {text}"
                    );
                    assert!(
                        !text.contains(locale.text("install_verification_compressed_not_compared")),
                        "{locale:?} must not use the compressed wording: {text}"
                    );
                }
                // The line is informational: no code prefix, no failure wording.
                assert!(text.contains(locale.text("install_verification")), "{text}");
                assert!(!text.contains("install_unsupported"), "{text}");
                assert!(!text.contains("DOES NOT match"), "{text}");

                // The JSON was already correct and is untouched: the same run
                // serializes with the state and the detail the text now agrees with.
                let json = serde_json::to_value(&shaped).expect("serialize the result");
                assert_eq!(
                    json["verification"]["state"],
                    serde_json::json!("unsupported"),
                    "{json}"
                );
                assert!(
                    json["verification"]["detail"]
                        .as_str()
                        .is_some_and(|detail| detail.contains(if observed {
                            "compressed"
                        } else {
                            "cannot re-read"
                        })),
                    "{json}"
                );
            }
        }
    }

    /// The board's misleading line, at the CLI: a re-read that failed must not be
    /// printed as a mismatch, while a genuine mismatch keeps its wording.
    #[test]
    fn a_failed_reread_is_not_printed_as_a_mismatch() {
        let scratch = install_scratch("cli-reread-failure");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(true);
        let (result, _) = run_install(&manager, "sda", &image, Some("sda"), "", false);
        let result = result.expect("the write runs");

        for locale in [Locale::En, Locale::ZhCn] {
            // What the board saw: the re-read failed, the write had completed.
            let mut failed = result.clone();
            failed.verification = rsetup_core::SystemInstallVerification {
                state: SystemInstallVerificationState::Unsupported,
                expected: Vec::new(),
                observed: None,
                detail: Some(
                    "the kernel could not re-read the partition table of /dev/sda after the image was written, so the write could not be verified: unable to write the system image: the kernel did not re-read /dev/sda: Invalid argument (os error 22)".into(),
                ),
            };
            let mut out = Vec::new();
            write_install_result(&failed, locale, &mut out).expect("render the result");
            let text = String::from_utf8(out).expect("utf-8");
            assert!(
                text.contains("could not re-read the partition table of /dev/sda"),
                "{locale:?}: {text}"
            );
            assert!(
                text.contains("after the image was written"),
                "{locale:?} must say the write completed: {text}"
            );
            assert!(
                text.contains("Invalid argument (os error 22)"),
                "{locale:?} must carry the kernel's reason: {text}"
            );
            assert!(
                !text.contains(locale.text("install_verification_mismatched")),
                "{locale:?}: the board's misleading line must be gone: {text}"
            );
            assert!(!text.contains("DOES NOT match"), "{locale:?}: {text}");
            assert!(
                !text.contains("differs from the image"),
                "{locale:?}: {text}"
            );

            // The genuine mismatch — the table was read and differs — is unchanged.
            let mut mismatched = result.clone();
            mismatched.verification = rsetup_core::SystemInstallVerification {
                state: SystemInstallVerificationState::Mismatched,
                expected: vec![rsetup_core::SystemInstallPartition {
                    name: "sda1".into(),
                    number: 1,
                    start_bytes: 1024 * 1024,
                    size_bytes: 4 * 1024 * 1024,
                    mount_point: None,
                }],
                observed: Some(vec![rsetup_core::SystemInstallPartition {
                    name: "p1".into(),
                    number: 1,
                    start_bytes: 4096,
                    size_bytes: 8 * 1024 * 1024,
                    mount_point: None,
                }]),
                detail: Some("the re-read partition table differs from the image".into()),
            };
            let mut out = Vec::new();
            write_install_result(&mismatched, locale, &mut out).expect("render the result");
            let text = String::from_utf8(out).expect("utf-8");
            assert!(
                text.contains(locale.text("install_verification_mismatched")),
                "{locale:?} keeps the mismatch wording: {text}"
            );
            assert!(
                text.contains("differs from the image"),
                "{locale:?}: {text}"
            );
            assert!(
                !text.contains("could not re-read"),
                "{locale:?}: the two cases stay distinguishable: {text}"
            );
        }
    }

    #[test]
    fn install_plan_preview_is_localized() {
        let scratch = install_scratch("preview");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);
        let plan = manager
            .plan(&install_request_for("sda", &image))
            .expect("plan");
        for locale in [Locale::En, Locale::ZhCn] {
            let mut out = Vec::new();
            write_install_plan(&plan, locale, &mut out).expect("render plan");
            let text = String::from_utf8(out).expect("utf-8");
            assert!(text.contains("/dev/sda"), "{locale:?}: {text}");
            assert!(text.contains(&plan.image.sha256_prefix), "{locale:?}");
            assert!(text.contains("UFS"), "{locale:?}: {text}");
            assert!(
                text.contains(&format_bytes(plan.target.size_bytes)),
                "{locale:?}: {text}"
            );
            let mut targets_out = Vec::new();
            let targets = manager.targets().expect("demo targets");
            write_install_targets(&targets, locale, &mut targets_out).expect("render targets");
            let targets_text = String::from_utf8(targets_out).expect("utf-8");
            assert!(targets_text.contains("/dev/sda"), "{targets_text}");
            assert!(targets_text.contains("/dev/mmcblk1"), "{targets_text}");
        }
    }
}
