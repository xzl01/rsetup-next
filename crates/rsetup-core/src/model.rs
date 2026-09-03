use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProbeMode {
    Auto,
    Live,
    Demo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIdentity {
    pub id: String,
    pub hostname: String,
    pub product: String,
    pub soc: String,
    #[serde(default)]
    pub soc_vendor: Option<String>,
    pub operating_system: String,
    pub kernel: String,
    pub architecture: String,
    pub mode: ProbeMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSnapshot {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub identity: DeviceIdentity,
    pub metrics: MetricSet,
    pub storage: Vec<StorageMetric>,
    pub interfaces: Vec<NetworkInterface>,
    pub services: Vec<ServiceSummary>,
    pub capabilities: Vec<Capability>,
    pub alerts: Vec<Alert>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricSet {
    pub cpu_percent: f32,
    pub load_average: [f32; 3],
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub temperature_c: Option<f32>,
    pub uptime_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageMetric {
    pub name: String,
    pub mount_point: String,
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub removable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInterface {
    pub name: String,
    pub kind: String,
    pub state: String,
    pub address: Option<String>,
    pub received_bytes: u64,
    pub transmitted_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceState {
    Active,
    Inactive,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSummary {
    pub id: String,
    pub label: String,
    pub state: ServiceState,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub id: String,
    pub label: String,
    pub available: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub id: String,
    pub level: AlertLevel,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Safe,
    Guarded,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionSpec {
    pub id: String,
    pub title: String,
    pub description: String,
    pub category: String,
    pub risk: RiskLevel,
    pub requires_root: bool,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub estimated_seconds: u32,
    pub steps: Vec<String>,
    #[serde(skip)]
    pub(crate) command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ActionStatus {
    Planned,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionRun {
    pub id: String,
    pub action_id: String,
    pub action_title: String,
    pub status: ActionStatus,
    pub synthetic: bool,
    pub summary: String,
    pub output: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    pub id: String,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub synthetic: bool,
}
