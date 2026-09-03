use crate::{ActionRun, ActionStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MirrorProvider {
    pub id: String,
    pub name: String,
    pub location: String,
    pub system_endpoint: Option<String>,
    pub radxa_endpoint: Option<String>,
    pub official: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    Debian,
    DebianSecurity,
    Ubuntu,
    UbuntuPorts,
    Radxa,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceFileSummary {
    pub path: String,
    pub format: String,
    pub managed_entries: usize,
    pub kinds: Vec<SourceKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub distribution_id: String,
    pub distribution_name: String,
    pub architecture: String,
    pub source_revision: String,
    pub files: Vec<SourceFileSummary>,
    pub current_system_provider: Option<String>,
    pub current_radxa_provider: Option<String>,
    pub providers: Vec<MirrorProvider>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceFileChange {
    pub path: String,
    pub replacements: usize,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePlan {
    pub provider: MirrorProvider,
    pub source_revision: String,
    pub plan_token: String,
    pub synthetic: bool,
    pub requires_root: bool,
    pub changes: Vec<SourceFileChange>,
    pub warnings: Vec<String>,
    pub will_refresh_package_index: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceApplyResult {
    pub run: ActionRun,
    pub plan: SourcePlan,
    pub backups: Vec<String>,
    pub rolled_back: bool,
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("unknown mirror provider: {0}")]
    UnknownProvider(String),
    #[error("APT source management is unavailable: {0}")]
    Unsupported(String),
    #[error("confirmation is required before changing APT sources")]
    ConfirmationRequired,
    #[error("preview the APT source change before applying it")]
    PlanRequired,
    #[error("the APT source files changed after preview; create a fresh plan")]
    StalePlan,
    #[error("changing APT sources requires root privileges")]
    RootRequired,
    #[error("unable to manage APT sources: {0}")]
    Io(String),
}

#[derive(Debug, Clone)]
pub(crate) struct SourceManager {
    root: PathBuf,
    synthetic: bool,
}

#[derive(Debug)]
pub(crate) struct LiveApplyOutcome {
    pub plan: SourcePlan,
    pub backups: Vec<String>,
    pub rolled_back: bool,
    pub status: ActionStatus,
    pub summary: String,
    pub output: Option<String>,
}

#[derive(Clone, Copy)]
struct ProviderDefinition {
    id: &'static str,
    name: &'static str,
    location: &'static str,
    system_base: Option<&'static str>,
    radxa_base: Option<&'static str>,
    official: bool,
}

const PROVIDERS: &[ProviderDefinition] = &[
    ProviderDefinition {
        id: "official",
        name: "Distribution official",
        location: "Global",
        system_base: Some("distribution-default"),
        radxa_base: Some("https://radxa-repo.github.io"),
        official: true,
    },
    ProviderDefinition {
        id: "ustc",
        name: "USTC Mirrors",
        location: "Hefei, CN",
        system_base: Some("https://mirrors.ustc.edu.cn"),
        radxa_base: None,
        official: false,
    },
    ProviderDefinition {
        id: "tuna",
        name: "TUNA Mirrors",
        location: "Beijing, CN",
        system_base: Some("https://mirrors.tuna.tsinghua.edu.cn"),
        radxa_base: None,
        official: false,
    },
    ProviderDefinition {
        id: "cqu",
        name: "CQU Mirrors",
        location: "Chongqing, CN",
        system_base: Some("https://mirrors.cqu.edu.cn"),
        radxa_base: Some("https://mirrors.cqu.edu.cn/radxa-deb"),
        official: false,
    },
    ProviderDefinition {
        id: "lzu",
        name: "LZU Mirrors",
        location: "Lanzhou, CN",
        system_base: Some("https://mirrors.lzu.edu.cn"),
        radxa_base: Some("https://mirrors.lzu.edu.cn/radxa-deb"),
        official: false,
    },
    ProviderDefinition {
        id: "hust",
        name: "HUST Mirrors",
        location: "Wuhan, CN",
        system_base: Some("https://mirrors.hust.edu.cn"),
        radxa_base: Some("https://mirrors.hust.edu.cn/radxa-deb"),
        official: false,
    },
    ProviderDefinition {
        id: "sdu",
        name: "SDU Mirrors",
        location: "Jinan, CN",
        system_base: Some("https://mirrors.sdu.edu.cn"),
        radxa_base: Some("https://mirrors.sdu.edu.cn/radxa-deb"),
        official: false,
    },
    ProviderDefinition {
        id: "nju",
        name: "NJU Mirrors",
        location: "Nanjing, CN",
        system_base: Some("https://mirror.nju.edu.cn"),
        radxa_base: Some("https://mirror.nju.edu.cn/radxa-deb"),
        official: false,
    },
    ProviderDefinition {
        id: "nyist",
        name: "NYIST Mirrors",
        location: "Nanyang, CN",
        system_base: Some("https://mirror.nyist.edu.cn"),
        radxa_base: Some("https://mirror.nyist.edu.cn/radxa-deb"),
        official: false,
    },
    ProviderDefinition {
        id: "aghost",
        name: "Aghost Mirrors",
        location: "China",
        system_base: None,
        radxa_base: Some("https://mirrors.aghost.cn/radxa-deb"),
        official: false,
    },
];

#[derive(Debug, Clone)]
struct SourceDocument {
    actual_path: Option<PathBuf>,
    display_path: String,
    format: &'static str,
    content: String,
}

#[derive(Debug)]
struct PendingChange {
    document: SourceDocument,
    updated: String,
    public: SourceFileChange,
}

impl SourceManager {
    pub(crate) fn new(synthetic: bool) -> Self {
        Self {
            root: PathBuf::from("/"),
            synthetic,
        }
    }

    #[cfg(test)]
    fn at_root(root: PathBuf) -> Self {
        Self {
            root,
            synthetic: false,
        }
    }

    pub(crate) fn status(&self) -> Result<SourceStatus, SourceError> {
        let (distribution_id, distribution_name) = self.distribution();
        let architecture = if self.synthetic {
            "aarch64".to_string()
        } else {
            std::env::consts::ARCH.to_string()
        };
        let documents = self.documents()?;
        let source_revision = source_revision(&documents);
        let mut files = Vec::new();
        let mut system_providers = BTreeSet::new();
        let mut radxa_providers = BTreeSet::new();

        for document in &documents {
            let entries = inspect_document(document, &distribution_id);
            if entries.is_empty() {
                continue;
            }
            let mut kinds = BTreeSet::new();
            for (kind, provider) in &entries {
                kinds.insert(*kind);
                if *kind == SourceKind::Radxa {
                    radxa_providers.insert(provider.clone());
                } else {
                    system_providers.insert(provider.clone());
                }
            }
            files.push(SourceFileSummary {
                path: document.display_path.clone(),
                format: document.format.into(),
                managed_entries: entries.len(),
                kinds: kinds.into_iter().collect(),
            });
        }

        let supported_distribution = matches!(distribution_id.as_str(), "debian" | "ubuntu");
        Ok(SourceStatus {
            collected_at: Utc::now(),
            synthetic: self.synthetic,
            supported: !files.is_empty() && supported_distribution,
            distribution_id,
            distribution_name,
            architecture,
            source_revision,
            files,
            current_system_provider: collapsed_provider(system_providers),
            current_radxa_provider: collapsed_provider(radxa_providers),
            providers: provider_catalog(),
        })
    }

    pub(crate) fn plan(&self, provider_id: &str) -> Result<SourcePlan, SourceError> {
        let (plan, _) = self.build_plan(provider_id)?;
        Ok(plan)
    }

    pub(crate) fn apply_live(
        &self,
        provider_id: &str,
        plan_token: &str,
    ) -> Result<LiveApplyOutcome, SourceError> {
        let (plan, pending) = self.build_plan(provider_id)?;
        verify_plan_token(&plan, plan_token)?;
        if pending.is_empty() {
            return Ok(LiveApplyOutcome {
                plan,
                backups: Vec::new(),
                rolled_back: false,
                status: ActionStatus::Succeeded,
                summary: "APT sources already use the selected mirror.".into(),
                output: None,
            });
        }

        let mut written = Vec::new();
        let mut backups = Vec::new();
        for change in &pending {
            let path = change
                .document
                .actual_path
                .as_ref()
                .ok_or_else(|| SourceError::Io("synthetic source cannot be written".into()))?;
            let current = fs::read_to_string(path)
                .map_err(|error| SourceError::Io(format!("{}: {error}", path.display())))?;
            if current != change.document.content {
                restore_written(&written);
                return Err(SourceError::StalePlan);
            }
            let backup = backup_path(path);
            if let Err(error) = fs::copy(path, &backup) {
                restore_written(&written);
                return Err(SourceError::Io(format!(
                    "unable to back up {}: {error}",
                    change.document.display_path
                )));
            }
            backups.push(display_for_root(&self.root, &backup));
            if let Err(error) = atomic_write(path, change.updated.as_bytes()) {
                let _ = fs::copy(&backup, path);
                restore_written(&written);
                return Err(SourceError::Io(format!(
                    "unable to write {}: {error}",
                    change.document.display_path
                )));
            }
            written.push((path.clone(), backup));
        }

        let update = apt_update();
        match update {
            Ok(output) => Ok(LiveApplyOutcome {
                plan,
                backups,
                rolled_back: false,
                status: ActionStatus::Succeeded,
                summary: "APT sources changed and package metadata refreshed.".into(),
                output,
            }),
            Err(message) => {
                restore_written(&written);
                Ok(LiveApplyOutcome {
                    plan,
                    backups,
                    rolled_back: true,
                    status: ActionStatus::Failed,
                    summary:
                        "Package metadata refresh failed; the previous APT sources were restored."
                            .into(),
                    output: Some(message),
                })
            }
        }
    }

    fn build_plan(
        &self,
        provider_id: &str,
    ) -> Result<(SourcePlan, Vec<PendingChange>), SourceError> {
        let definition = PROVIDERS
            .iter()
            .find(|provider| provider.id == provider_id)
            .copied()
            .ok_or_else(|| SourceError::UnknownProvider(provider_id.into()))?;
        let (distribution_id, _) = self.distribution();
        if !matches!(distribution_id.as_str(), "debian" | "ubuntu") {
            return Err(SourceError::Unsupported(format!(
                "distribution {distribution_id} is not Debian or Ubuntu"
            )));
        }
        let architecture = if self.synthetic {
            "aarch64"
        } else {
            std::env::consts::ARCH
        };
        let documents = self.documents()?;
        let source_revision = source_revision(&documents);
        let mut pending = Vec::new();
        for document in documents {
            let (updated, public) =
                transform_document(&document, &distribution_id, architecture, definition);
            if let Some(public) = public {
                pending.push(PendingChange {
                    document,
                    updated,
                    public,
                });
            }
        }
        let mut warnings = Vec::new();
        if definition.system_base.is_none() {
            warnings.push("radxa_only".into());
        }
        if definition.radxa_base.is_none() {
            warnings.push("system_only".into());
        }
        if pending.is_empty() {
            let has_managed_sources = !self.status()?.files.is_empty();
            warnings.push(
                if has_managed_sources {
                    "already_selected"
                } else {
                    "no_managed_sources"
                }
                .into(),
            );
        }
        if self.synthetic {
            warnings.push("dry_run".into());
        }
        let plan_token = plan_token(definition.id, &source_revision, &pending);
        let plan = SourcePlan {
            provider: definition.public(),
            source_revision,
            plan_token,
            synthetic: self.synthetic,
            requires_root: true,
            changes: pending.iter().map(|item| item.public.clone()).collect(),
            warnings,
            will_refresh_package_index: !pending.is_empty(),
        };
        Ok((plan, pending))
    }

    fn distribution(&self) -> (String, String) {
        if self.synthetic {
            return ("debian".into(), "Debian GNU/Linux 12 (demo)".into());
        }
        let content = fs::read_to_string(self.root.join("etc/os-release")).unwrap_or_default();
        let id = os_release_value(&content, "ID").unwrap_or_else(|| "unknown".into());
        let name = os_release_value(&content, "PRETTY_NAME").unwrap_or_else(|| id.clone());
        (id.to_ascii_lowercase(), name)
    }

    fn documents(&self) -> Result<Vec<SourceDocument>, SourceError> {
        if self.synthetic {
            return Ok(vec![
                SourceDocument {
                    actual_path: None,
                    display_path: "/etc/apt/sources.list.d/debian.sources".into(),
                    format: "deb822",
                    content: "Types: deb\nURIs: https://deb.debian.org/debian\nSuites: bookworm bookworm-updates\nComponents: main contrib non-free-firmware\n\nTypes: deb\nURIs: https://security.debian.org/debian-security\nSuites: bookworm-security\nComponents: main contrib non-free-firmware\n".into(),
                },
                SourceDocument {
                    actual_path: None,
                    display_path: "/etc/apt/sources.list.d/radxa.list".into(),
                    format: "list",
                    content: "deb [signed-by=/usr/share/keyrings/radxa-archive-keyring.gpg] https://radxa-repo.github.io/bookworm bookworm main\n".into(),
                },
            ]);
        }

        let apt = self.root.join("etc/apt");
        let mut paths = Vec::new();
        let main = apt.join("sources.list");
        if main.is_file() {
            paths.push(main);
        }
        let directory = apt.join("sources.list.d");
        if directory.is_dir() {
            let entries = fs::read_dir(&directory)
                .map_err(|error| SourceError::Io(format!("{}: {error}", directory.display())))?;
            for entry in entries {
                let path = entry
                    .map_err(|error| SourceError::Io(error.to_string()))?
                    .path();
                if matches!(
                    path.extension().and_then(|value| value.to_str()),
                    Some("list" | "sources")
                ) && path.is_file()
                {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        let mut documents = Vec::new();
        for path in paths {
            if fs::symlink_metadata(&path)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                continue;
            }
            let content = fs::read_to_string(&path)
                .map_err(|error| SourceError::Io(format!("{}: {error}", path.display())))?;
            let format = if path.extension().and_then(|value| value.to_str()) == Some("sources") {
                "deb822"
            } else {
                "list"
            };
            documents.push(SourceDocument {
                actual_path: Some(path.clone()),
                display_path: display_for_root(&self.root, &path),
                format,
                content,
            });
        }
        Ok(documents)
    }
}

impl ProviderDefinition {
    fn public(self) -> MirrorProvider {
        MirrorProvider {
            id: self.id.into(),
            name: self.name.into(),
            location: self.location.into(),
            system_endpoint: self.system_base.map(str::to_string),
            radxa_endpoint: self.radxa_base.map(str::to_string),
            official: self.official,
        }
    }
}

pub fn provider_catalog() -> Vec<MirrorProvider> {
    PROVIDERS
        .iter()
        .copied()
        .map(ProviderDefinition::public)
        .collect()
}

fn inspect_document(document: &SourceDocument, distribution_id: &str) -> Vec<(SourceKind, String)> {
    let mut result = Vec::new();
    for line in eligible_lines(document) {
        for (_, _, uri) in uri_tokens(line) {
            if let Some((kind, provider, _)) = classify_uri(uri, distribution_id) {
                result.push((kind, provider.into()));
            }
        }
    }
    result
}

fn transform_document(
    document: &SourceDocument,
    distribution_id: &str,
    architecture: &str,
    provider: ProviderDefinition,
) -> (String, Option<SourceFileChange>) {
    let source_lines: Vec<&str> = document.content.split_inclusive('\n').collect();
    let security = security_hints(&source_lines, document.format);
    let mut output = String::with_capacity(document.content.len() + 128);
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut replacements = 0;

    for (index, line) in source_lines.iter().enumerate() {
        if !is_eligible_line(line, document.format) {
            output.push_str(line);
            continue;
        }
        let mut updated = (*line).to_string();
        let mut edits = Vec::new();
        for (start, end, uri) in uri_tokens(line) {
            if let Some((kind, _, suffix)) = classify_uri(uri, distribution_id)
                && let Some(target) = target_uri(
                    provider,
                    kind,
                    distribution_id,
                    architecture,
                    security.get(index).copied().unwrap_or(false),
                    &suffix,
                )
                && target != uri.trim_end_matches('/')
            {
                edits.push((start, end, target));
            }
        }
        for (start, end, target) in edits.iter().rev() {
            updated.replace_range(*start..*end, target);
        }
        if !edits.is_empty() {
            replacements += edits.len();
            before.push(line.trim_end().to_string());
            after.push(updated.trim_end().to_string());
        }
        output.push_str(&updated);
    }
    if !document.content.ends_with('\n') && output.ends_with('\n') {
        output.pop();
    }
    let change = (replacements > 0).then(|| SourceFileChange {
        path: document.display_path.clone(),
        replacements,
        before,
        after,
    });
    (output, change)
}

fn eligible_lines(document: &SourceDocument) -> impl Iterator<Item = &str> {
    document
        .content
        .lines()
        .filter(|line| is_eligible_line(line, document.format))
}

fn is_eligible_line(line: &str, format: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false;
    }
    if format == "deb822" {
        trimmed
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("uris:"))
    } else {
        trimmed == "deb"
            || trimmed == "deb-src"
            || trimmed.starts_with("deb ")
            || trimmed.starts_with("deb-src ")
    }
}

fn uri_tokens(line: &str) -> Vec<(usize, usize, &str)> {
    let mut result = Vec::new();
    let mut start = None;
    for (index, character) in line.char_indices() {
        if character.is_whitespace() {
            if let Some(token_start) = start.take() {
                let token = &line[token_start..index];
                if token.starts_with("http://") || token.starts_with("https://") {
                    result.push((token_start, index, token));
                }
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(token_start) = start {
        let token = &line[token_start..];
        if token.starts_with("http://") || token.starts_with("https://") {
            result.push((token_start, line.len(), token));
        }
    }
    result
}

fn security_hints(lines: &[&str], format: &str) -> Vec<bool> {
    let mut hints = vec![false; lines.len()];
    if format != "deb822" {
        for (index, line) in lines.iter().enumerate() {
            hints[index] = line.contains("-security");
        }
        return hints;
    }
    let mut block_start = 0;
    for index in 0..=lines.len() {
        let boundary = index == lines.len() || lines[index].trim().is_empty();
        if !boundary {
            continue;
        }
        let is_security = lines[block_start..index].iter().any(|line| {
            line.trim_start()
                .to_ascii_lowercase()
                .starts_with("suites:")
                && line.contains("-security")
        });
        for hint in &mut hints[block_start..index] {
            *hint = is_security;
        }
        block_start = index.saturating_add(1);
    }
    hints
}

fn classify_uri(uri: &str, distribution_id: &str) -> Option<(SourceKind, &'static str, String)> {
    let normalized = uri.trim_end_matches('/');
    for provider in PROVIDERS {
        if let Some(base) = provider.radxa_base
            && base != "distribution-default"
            && let Some(suffix) = strip_uri_base(normalized, base)
        {
            return Some((SourceKind::Radxa, provider.id, suffix));
        }
    }

    let (host, path) = host_and_path(normalized)?;
    if host == "radxa-repo.github.io" {
        return Some((SourceKind::Radxa, "official", path.trim_matches('/').into()));
    }
    let provider = provider_for_system_host(host)?;
    let kind = match distribution_id {
        "debian" if path_has_segment(path, "debian-security") => SourceKind::DebianSecurity,
        "debian" if path_has_segment(path, "debian") => SourceKind::Debian,
        "ubuntu" if path_has_segment(path, "ubuntu-ports") => SourceKind::UbuntuPorts,
        "ubuntu" if path_has_segment(path, "ubuntu") => SourceKind::Ubuntu,
        _ => return None,
    };
    Some((kind, provider, String::new()))
}

fn target_uri(
    provider: ProviderDefinition,
    kind: SourceKind,
    distribution_id: &str,
    architecture: &str,
    security: bool,
    suffix: &str,
) -> Option<String> {
    if kind == SourceKind::Radxa {
        let base = provider.radxa_base?;
        return Some(join_uri(base, suffix));
    }
    let base = provider.system_base?;
    if provider.official {
        return match distribution_id {
            "debian" if kind == SourceKind::DebianSecurity => {
                Some("https://security.debian.org/debian-security".into())
            }
            "debian" => Some("https://deb.debian.org/debian".into()),
            "ubuntu" if is_ports_architecture(architecture) => {
                Some("https://ports.ubuntu.com/ubuntu-ports".into())
            }
            "ubuntu" if security => Some("https://security.ubuntu.com/ubuntu".into()),
            "ubuntu" => Some("https://archive.ubuntu.com/ubuntu".into()),
            _ => None,
        };
    }
    let repository = match kind {
        SourceKind::Debian => "debian",
        SourceKind::DebianSecurity => "debian-security",
        SourceKind::Ubuntu => "ubuntu",
        SourceKind::UbuntuPorts => "ubuntu-ports",
        SourceKind::Radxa => return None,
    };
    Some(join_uri(base, repository))
}

fn provider_for_system_host(host: &str) -> Option<&'static str> {
    match host {
        "deb.debian.org"
        | "security.debian.org"
        | "ftp.debian.org"
        | "archive.ubuntu.com"
        | "security.ubuntu.com"
        | "ports.ubuntu.com"
        | "deb.ubuntu.com" => Some("official"),
        _ => PROVIDERS.iter().find_map(|provider| {
            let base = provider.system_base?;
            let (candidate, _) = host_and_path(base)?;
            (candidate == host).then_some(provider.id)
        }),
    }
}

fn host_and_path(uri: &str) -> Option<(&str, &str)> {
    let rest = uri
        .strip_prefix("https://")
        .or_else(|| uri.strip_prefix("http://"))?;
    Some(rest.split_once('/').unwrap_or((rest, "")))
}

fn strip_uri_base(uri: &str, base: &str) -> Option<String> {
    let suffix = uri.strip_prefix(base.trim_end_matches('/'))?;
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return None;
    }
    Some(suffix.trim_matches('/').into())
}

fn path_has_segment(path: &str, segment: &str) -> bool {
    path.split('/').any(|value| value == segment)
}

fn join_uri(base: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        base.trim_end_matches('/').into()
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            suffix.trim_matches('/')
        )
    }
}

fn is_ports_architecture(architecture: &str) -> bool {
    matches!(
        architecture,
        "aarch64" | "arm" | "armv6" | "armv7" | "riscv64" | "ppc64" | "s390x"
    )
}

fn collapsed_provider(providers: BTreeSet<String>) -> Option<String> {
    match providers.len() {
        0 => None,
        1 => providers.into_iter().next(),
        _ => Some("mixed".into()),
    }
}

fn verify_plan_token(plan: &SourcePlan, supplied: &str) -> Result<(), SourceError> {
    if supplied.trim().is_empty() {
        return Err(SourceError::PlanRequired);
    }
    if supplied != plan.plan_token {
        return Err(SourceError::StalePlan);
    }
    Ok(())
}

fn source_revision(documents: &[SourceDocument]) -> String {
    let mut fingerprint = Fingerprint::new();
    for document in documents {
        fingerprint.update(document.display_path.as_bytes());
        fingerprint.update(document.format.as_bytes());
        fingerprint.update(document.content.as_bytes());
    }
    fingerprint.finish("sources-v1")
}

fn plan_token(provider_id: &str, source_revision: &str, pending: &[PendingChange]) -> String {
    let mut fingerprint = Fingerprint::new();
    fingerprint.update(provider_id.as_bytes());
    fingerprint.update(source_revision.as_bytes());
    for change in pending {
        fingerprint.update(change.document.display_path.as_bytes());
        fingerprint.update(change.updated.as_bytes());
    }
    fingerprint.finish("plan-v1")
}

struct Fingerprint {
    left: u64,
    right: u64,
}

impl Fingerprint {
    fn new() -> Self {
        Self {
            left: 0xcbf2_9ce4_8422_2325,
            right: 0x8422_2325_cbf2_9ce4,
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.update_raw(&(bytes.len() as u64).to_le_bytes());
        self.update_raw(bytes);
    }

    fn update_raw(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.left ^= u64::from(*byte);
            self.left = self.left.wrapping_mul(0x0000_0100_0000_01b3);
            self.right ^= u64::from(*byte).rotate_left(1);
            self.right = self.right.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self, prefix: &str) -> String {
        format!("{prefix}-{:016x}{:016x}", self.left, self.right)
    }
}

fn os_release_value(content: &str, key: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate == key).then(|| value.trim().trim_matches('"').to_string())
    })
}

fn display_for_root(root: &Path, path: &Path) -> String {
    if root == Path::new("/") {
        return path.display().to_string();
    }
    path.strip_prefix(root)
        .map(|relative| format!("/{}", relative.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn backup_path(path: &Path) -> PathBuf {
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("sources");
    path.with_file_name(format!(
        "{name}.rsetup-backup-{stamp}-{}",
        Uuid::new_v4().simple()
    ))
}

fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("refusing to replace a symbolic link"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("source file has no parent directory"))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("sources");
    let temporary = parent.join(format!(".{name}.rsetup-{}.tmp", Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(content)?;
        file.set_permissions(metadata.permissions())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Ok(directory) = OpenOptions::new().read(true).open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn restore_written(written: &[(PathBuf, PathBuf)]) {
    for (path, backup) in written.iter().rev() {
        if let Ok(content) = fs::read(backup) {
            let _ = atomic_write(path, &content);
        }
    }
}

fn apt_update() -> Result<Option<String>, String> {
    let program = ["/usr/bin/apt-get", "/bin/apt-get"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .ok_or_else(|| "apt-get was not found on this host".to_string())?;
    let result = Command::new(program)
        .arg("update")
        .output()
        .map_err(|error| format!("unable to start apt-get update: {error}"))?;
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let bounded = output.trim().chars().take(8_000).collect::<String>();
    if result.status.success() {
        Ok((!bounded.is_empty()).then_some(bounded))
    } else {
        Err(format!(
            "apt-get update exited with {}\n{bounded}",
            result.status
        ))
    }
}

pub(crate) fn source_run(
    status: ActionStatus,
    synthetic: bool,
    summary: String,
    output: Option<String>,
    started_at: DateTime<Utc>,
) -> ActionRun {
    ActionRun {
        id: Uuid::new_v4().to_string(),
        action_id: "system.change-sources".into(),
        action_title: "Change package mirrors".into(),
        status,
        synthetic,
        summary,
        output,
        started_at,
        finished_at: Some(Utc::now()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_changes_deb822_and_radxa_without_touching_third_party() {
        let manager = SourceManager::new(true);
        let plan = manager.plan("cqu").expect("demo plan");
        assert_eq!(plan.changes.len(), 2);
        assert_eq!(
            plan.changes
                .iter()
                .map(|change| change.replacements)
                .sum::<usize>(),
            3
        );
        assert!(
            plan.changes
                .iter()
                .flat_map(|change| &change.after)
                .any(|line| line.contains("mirrors.cqu.edu.cn/debian-security"))
        );
        assert!(
            plan.changes
                .iter()
                .flat_map(|change| &change.after)
                .any(|line| line.contains("mirrors.cqu.edu.cn/radxa-deb/bookworm"))
        );
    }

    #[test]
    fn custom_repository_is_not_managed() {
        let document = SourceDocument {
            actual_path: None,
            display_path: "/etc/apt/sources.list".into(),
            format: "list",
            content: "deb https://packages.example.com/debian stable main\n".into(),
        };
        let (_, change) = transform_document(
            &document,
            "debian",
            "aarch64",
            PROVIDERS
                .iter()
                .find(|item| item.id == "ustc")
                .copied()
                .unwrap(),
        );
        assert!(change.is_none());
    }

    #[test]
    fn official_ubuntu_restore_uses_ports_on_arm_and_security_on_amd64() {
        let arm = SourceDocument {
            actual_path: None,
            display_path: "/etc/apt/sources.list".into(),
            format: "list",
            content: "deb https://mirrors.cqu.edu.cn/ubuntu-ports noble main\n".into(),
        };
        let (_, arm_change) = transform_document(&arm, "ubuntu", "aarch64", PROVIDERS[0]);
        assert!(arm_change.unwrap().after[0].contains("https://ports.ubuntu.com/ubuntu-ports"));

        let amd64 = SourceDocument {
            actual_path: None,
            display_path: "/etc/apt/sources.list".into(),
            format: "list",
            content: "deb https://mirrors.cqu.edu.cn/ubuntu noble-security main\n".into(),
        };
        let (_, amd64_change) = transform_document(&amd64, "ubuntu", "x86_64", PROVIDERS[0]);
        assert!(amd64_change.unwrap().after[0].contains("https://security.ubuntu.com/ubuntu"));
    }

    #[test]
    fn system_only_provider_preserves_radxa_entries() {
        let manager = SourceManager::new(true);
        let plan = manager.plan("ustc").expect("demo plan");
        assert_eq!(plan.changes.len(), 1);
        assert!(plan.warnings.iter().any(|warning| warning == "system_only"));
        assert!(
            plan.changes
                .iter()
                .all(|change| !change.path.contains("radxa"))
        );
    }

    #[test]
    fn source_status_reads_a_rooted_fixture() {
        let root = std::env::temp_dir().join(format!("rsetup-source-test-{}", Uuid::new_v4()));
        let directory = root.join("etc/apt/sources.list.d");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            root.join("etc/os-release"),
            "ID=debian\nPRETTY_NAME=\"Debian Test\"\n",
        )
        .unwrap();
        fs::write(
            directory.join("debian.sources"),
            "Types: deb\nURIs: https://deb.debian.org/debian\nSuites: stable\nComponents: main\n",
        )
        .unwrap();
        let status = SourceManager::at_root(root.clone()).status().unwrap();
        assert!(status.supported);
        assert_eq!(status.current_system_provider.as_deref(), Some("official"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_rejects_a_plan_after_any_source_file_change() {
        let root = std::env::temp_dir().join(format!("rsetup-source-test-{}", Uuid::new_v4()));
        let directory = root.join("etc/apt/sources.list.d");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            root.join("etc/os-release"),
            "ID=debian\nPRETTY_NAME=\"Debian Test\"\n",
        )
        .unwrap();
        let source_path = directory.join("debian.sources");
        fs::write(
            &source_path,
            "Types: deb\nURIs: https://deb.debian.org/debian\nSuites: stable\nComponents: main\n",
        )
        .unwrap();
        let manager = SourceManager::at_root(root.clone());
        let plan = manager.plan("cqu").unwrap();
        fs::write(
            &source_path,
            "Types: deb\nURIs: https://deb.debian.org/debian\nSuites: stable\nComponents: main\n\n# changed after preview\n",
        )
        .unwrap();

        assert!(matches!(
            manager.apply_live("cqu", &plan.plan_token),
            Err(SourceError::StalePlan)
        ));
        assert!(
            fs::read_to_string(&source_path)
                .unwrap()
                .contains("changed after preview")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
