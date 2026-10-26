use super::{InstallFocus, InstallStatus};
use crate::i18n::Locale;
use crate::tui::app::App;
use crate::tui::theme::{AMBER, BONE, CORAL, INK, MUTED, SIGNAL, instrument};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use rsetup_core::{
    SystemInstallApplyResult, SystemInstallPlan, SystemInstallTarget,
    SystemInstallVerificationState,
};

fn install_device_kind_label(locale: Locale, kind: rsetup_core::BlockDeviceKind) -> &'static str {
    locale.install_device_kind(kind)
}

fn install_target_notes(target: &SystemInstallTarget, locale: Locale) -> String {
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
    if notes.is_empty() {
        String::new()
    } else {
        format!(" · {}", notes.join(" · "))
    }
}

/// The consequence list, rendered from a core plan.
fn install_plan_lines<'a>(plan: &SystemInstallPlan, locale: Locale) -> Vec<Line<'a>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", locale.text("install_target")),
                Style::default().fg(MUTED),
            ),
            Span::styled(plan.target.path.clone(), Style::default().fg(BONE)),
            Span::styled(
                format!(
                    " — {} ({})",
                    install_device_kind_label(locale, plan.target.kind),
                    plan.target.model
                ),
                Style::default().fg(MUTED),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!("{} ", locale.text("install_capacity")),
                Style::default().fg(MUTED),
            ),
            Span::styled(
                crate::format_bytes(plan.target.size_bytes),
                Style::default().fg(BONE),
            ),
            Span::styled(
                install_target_notes(&plan.target, locale),
                Style::default().fg(AMBER),
            ),
        ]),
    ];
    if plan.destroys.is_empty() {
        lines.push(Line::from(Span::styled(
            locale.text("install_no_partitions"),
            Style::default().fg(MUTED),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!(
                "{} ({})",
                locale.text("install_destroyed_partitions"),
                plan.destroys.len()
            ),
            Style::default().fg(CORAL),
        )));
        for partition in &plan.destroys {
            let mount = partition
                .mount_point
                .as_deref()
                .map(|point| format!(" · {} {point}", locale.text("install_partition_mounted_at")))
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  - {} ", partition.name),
                    Style::default().fg(BONE),
                ),
                Span::styled(
                    format!("{}{mount}", crate::format_bytes(partition.size_bytes)),
                    Style::default().fg(MUTED),
                ),
            ]));
        }
    }
    lines.push(Line::from(vec![
        Span::styled(
            format!("{} ", locale.text("install_image")),
            Style::default().fg(MUTED),
        ),
        Span::styled(plan.image.path.clone(), Style::default().fg(BONE)),
    ]));
    lines.push(Line::from(Span::styled(
        format!(
            "  {}: {:?} · {}: {} · {}: {}",
            locale.text("install_image_format"),
            plan.image.format,
            locale.text("install_image_size"),
            crate::format_bytes(plan.image.size_bytes),
            locale.text("install_image_uncompressed"),
            crate::format_bytes(plan.image.uncompressed_bytes)
        ),
        Style::default().fg(MUTED),
    )));
    lines.push(Line::from(Span::styled(
        format!(
            "  {}: {}",
            locale.text("install_image_sha256"),
            plan.image.sha256_prefix
        ),
        Style::default().fg(MUTED),
    )));
    let table = plan.image.partition_table.as_deref().unwrap_or("");
    lines.push(Line::from(Span::styled(
        format!(
            "  {}: {}",
            locale.text("install_partition_table"),
            if table.is_empty() {
                locale.text("install_partition_table_unknown").to_string()
            } else {
                table.to_uppercase()
            }
        ),
        Style::default().fg(MUTED),
    )));
    if plan.requires_root {
        lines.push(Line::from(Span::styled(
            locale.text("install_requires_root"),
            Style::default().fg(AMBER),
        )));
    }
    for warning in &plan.warnings {
        lines.push(Line::from(Span::styled(
            format!("! {}", locale.install_warning(warning)),
            Style::default().fg(CORAL),
        )));
    }
    lines
}

/// The wording for a finished write's verification line.
///
/// The core reports `Unsupported` for two different situations: a host that cannot
/// re-read a partition table at all, and a compressed image whose layout was never
/// parsed so the table it did re-read had nothing to be compared against. The
/// second one carries the table it read (`observed`), which is what tells them
/// apart — and saying "this host cannot re-read partition tables" there would be
/// simply wrong. The core's own `detail` is preferred whenever it gives one.
fn verification_text(result: &SystemInstallApplyResult, locale: Locale) -> String {
    let with_reason = |key: &str| match &result.verification.detail {
        Some(detail) => format!("{} ({detail})", locale.text(key)),
        None => locale.text(key).to_string(),
    };
    match result.verification.state {
        SystemInstallVerificationState::Matched => {
            locale.text("install_verification_matched").to_string()
        }
        SystemInstallVerificationState::Mismatched => {
            locale.text("install_verification_mismatched").to_string()
        }
        SystemInstallVerificationState::Unsupported => {
            if result.verification.observed.is_some() {
                with_reason("install_verification_compressed_not_compared")
            } else {
                with_reason("install_verification_unsupported")
            }
        }
    }
}

pub(in crate::tui) fn render_install_panel(frame: &mut Frame, app: &App, area: Rect) {
    let locale = app.locale;
    let panel = &app.install;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Step 1: the devices an image could be written to.
    lines.push(Line::from(Span::styled(
        locale.text("install_step_targets"),
        Style::default().fg(SIGNAL).add_modifier(Modifier::BOLD),
    )));
    if panel.targets.is_empty() {
        lines.push(Line::from(Span::styled(
            locale.text("install_no_targets"),
            Style::default().fg(AMBER),
        )));
    }
    for (index, target) in panel.targets.iter().enumerate() {
        let selected = index == panel.selected;
        let marker = if selected { "▸ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(
                marker.to_string(),
                Style::default().fg(if selected { SIGNAL } else { MUTED }),
            ),
            Span::styled(
                format!("{} ", target.path),
                Style::default()
                    .fg(if selected { BONE } else { MUTED })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                format!(
                    "{} {} · {}",
                    install_device_kind_label(locale, target.kind),
                    target.model,
                    crate::format_bytes(target.size_bytes)
                ),
                Style::default().fg(MUTED),
            ),
            Span::styled(
                install_target_notes(target, locale),
                Style::default().fg(AMBER),
            ),
        ]));
    }

    // Step 2: the image, and the hint to preview it.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        locale.text("install_step_image"),
        Style::default().fg(SIGNAL).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(vec![
        Span::styled(
            format!("{}: ", locale.text("install_field_image")),
            Style::default().fg(MUTED),
        ),
        Span::styled(
            if panel.image_path.is_empty() {
                locale.text("install_plan_hint").to_string()
            } else {
                panel.image_path.clone()
            },
            Style::default().fg(if panel.image_path.is_empty() {
                MUTED
            } else {
                BONE
            }),
        ),
        Span::styled(
            if matches!(panel.focus, InstallFocus::Image) {
                " ▌"
            } else {
                ""
            },
            Style::default().fg(SIGNAL),
        ),
    ]));

    // Step 3: every irreversible consequence of the loaded plan.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        locale.text("install_step_confirm"),
        Style::default().fg(SIGNAL).add_modifier(Modifier::BOLD),
    )));
    if let Some(plan) = &panel.plan {
        lines.extend(install_plan_lines(plan, locale));
    } else {
        lines.push(Line::from(Span::styled(
            locale.text("install_plan_hint"),
            Style::default().fg(MUTED),
        )));
    }

    // The typed confirmation, and whether the destructive key is enabled.
    lines.push(Line::from(""));
    let ready = panel.acknowledgement_ready();
    lines.push(Line::from(vec![
        Span::styled(
            format!("{}: ", locale.text("install_field_confirm")),
            Style::default().fg(MUTED),
        ),
        Span::styled(
            if panel.typed.is_empty() {
                "_____".to_string()
            } else {
                panel.typed.clone()
            },
            Style::default().fg(if ready { SIGNAL } else { BONE }),
        ),
        Span::styled(
            if matches!(panel.focus, InstallFocus::Confirm) {
                " ▌"
            } else {
                ""
            },
            Style::default().fg(SIGNAL),
        ),
    ]));
    if panel.plan.is_some() {
        let hint = if ready {
            if !panel.live_write {
                locale.text("install_dry_run_ready")
            } else {
                locale.text("install_ready")
            }
        } else {
            locale.text("install_disabled")
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(if ready { SIGNAL } else { CORAL }),
        )));
    }

    // Status: the core's own error, or the outcome of a write.
    if let Some(status) = &panel.status {
        match status {
            InstallStatus::Error(error) => {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{}: {}",
                        crate::system_install_error_code(error),
                        locale.install_error(error)
                    ),
                    Style::default().fg(CORAL),
                )));
            }
            InstallStatus::Helper { code, message } => {
                lines.push(Line::from(Span::styled(
                    format!("{code}: {message}"),
                    Style::default().fg(CORAL),
                )));
            }
            InstallStatus::Canceled => {
                // Amber like the informational lines, not coral: the write was
                // declined, so there is no error code to name.
                lines.push(Line::from(Span::styled(
                    if locale.is_zh() {
                        "已取消管理员授权，未写入任何数据。"
                    } else {
                        "Administrator authorization canceled. Nothing was written."
                    },
                    Style::default().fg(AMBER),
                )));
            }
            InstallStatus::Info(key) => {
                lines.push(Line::from(Span::styled(
                    locale.text(key),
                    Style::default().fg(AMBER),
                )));
            }
            InstallStatus::Planned => {
                if let Some(result) = &panel.result {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{}: {} — {}",
                            locale.text("install_planned"),
                            locale.text("install_target"),
                            crate::format_bytes(result.bytes_written),
                        ),
                        Style::default().fg(AMBER),
                    )));
                }
            }
            InstallStatus::Written => {
                if let Some(result) = &panel.result {
                    let verification = verification_text(result, locale);
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{}: {} — {}: {}",
                            locale.text("install_written"),
                            locale.text("install_target"),
                            crate::format_bytes(result.bytes_written),
                            verification
                        ),
                        Style::default().fg(SIGNAL),
                    )));
                    if result.run.synthetic {
                        lines.push(Line::from(Span::styled(
                            locale.text("install_synthetic_notice"),
                            Style::default().fg(AMBER),
                        )));
                    }
                }
            }
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(instrument(locale.text("install_panel_title")))
            .wrap(Wrap { trim: false })
            .scroll((0, 0)),
        area,
    );
}

pub(in crate::tui) fn render_install_footer(frame: &mut Frame, app: &App, area: Rect) {
    let status = if app.snapshot.synthetic {
        app.locale.text("synthetic_blocked")
    } else {
        app.locale.text("device_stable")
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" TAB ", Style::default().fg(INK).bg(BONE)),
            Span::styled(" J/K ", Style::default().fg(INK).bg(BONE)),
            Span::styled(" ENTER ", Style::default().fg(INK).bg(BONE)),
            Span::styled(" ESC ", Style::default().fg(INK).bg(BONE)),
            Span::raw(format!("  {}  ", app.locale.text("install_panel_footer"))),
            Span::styled(
                status.to_string(),
                Style::default().fg(if app.snapshot.synthetic {
                    AMBER
                } else {
                    SIGNAL
                }),
            ),
        ]))
        .style(Style::default().fg(MUTED))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(MUTED)),
        ),
        area,
    );
}
