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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryReadState {
    Available,
    Unsupported,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    #[default]
    Unknown,
    Healthy,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryErrorKind {
    PermissionDenied,
    Io,
    NvmeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryError {
    pub kind: TelemetryErrorKind,
    pub code: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryStatus {
    pub state: TelemetryReadState,
    pub error: Option<TelemetryError>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NvmeStatus {
    pub initialized: bool,
    pub devices: Vec<NvmeDevice>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NvmeDevice {
    pub name: String,
    pub path: String,
    pub model: String,
    pub serial: String,
    pub firmware: String,
    pub total_bytes: u64,
    pub smart: Option<NvmeSmartLog>,
    #[serde(default)]
    pub telemetry: TelemetryStatus,
    #[serde(default)]
    pub health_state: HealthState,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NvmeSmartLog {
    pub critical_warning: u8,
    pub warning_flags: Vec<String>,
    pub temperature_c: f32,
    pub available_spare_percent: u8,
    pub spare_threshold_percent: u8,
    pub percentage_used: u8,
    pub data_read_bytes: u64,
    pub data_written_bytes: u64,
    pub host_read_commands: u64,
    pub host_write_commands: u64,
    pub power_on_hours: u64,
    pub unsafe_shutdowns: u64,
    pub media_errors: u64,
    pub num_err_log_entries: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MmcHealth {
    pub pre_eol_info: u8,
    pub life_time_est_a_percent: Option<u8>,
    pub life_time_est_b_percent: Option<u8>,
    pub warning_flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MmcDevice {
    pub name: String,
    pub block_path: String,
    pub card_type: String,
    pub model: String,
    pub manufacturer: String,
    pub serial: String,
    pub firmware: String,
    pub total_bytes: u64,
    pub health: MmcHealth,
    #[serde(default)]
    pub telemetry: TelemetryStatus,
    #[serde(default)]
    pub health_state: HealthState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MmcStatus {
    pub initialized: bool,
    pub devices: Vec<MmcDevice>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatus {
    pub nvme: NvmeStatus,
    pub mmc: MmcStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_contract_exact_json_roundtrip_fixture() {
        let fixture_json = r#"{"name":"nvme0","path":"/dev/nvme0","model":"FIXTURE","serial":"test-only","firmware":"1","totalBytes":4096,"smart":null,"telemetry":{"state":"unavailable","error":{"kind":"permission_denied","code":13}},"healthState":"unknown"}"#;

        let dev: NvmeDevice = serde_json::from_str(fixture_json).expect("deserialize fixture JSON");
        assert!(dev.smart.is_none());
        assert_eq!(dev.health_state, HealthState::Unknown);
        assert_eq!(dev.telemetry.state, TelemetryReadState::Unavailable);
        assert_eq!(
            dev.telemetry.error,
            Some(TelemetryError {
                kind: TelemetryErrorKind::PermissionDenied,
                code: Some(13),
            })
        );

        let serialized = serde_json::to_string(&dev).expect("serialize NvmeDevice");
        assert!(serialized.contains(r#""smart":null"#));
        assert!(serialized.contains(r#""code":13"#));
        assert!(serialized.contains(r#""healthState":"unknown""#));
    }

    #[test]
    fn mmc_and_storage_models_serialize_and_deserialize() {
        let health = MmcHealth {
            pre_eol_info: 1,
            life_time_est_a_percent: Some(10),
            life_time_est_b_percent: Some(20),
            warning_flags: vec!["urgent".to_string()],
        };

        let device = MmcDevice {
            name: "mmcblk0".to_string(),
            block_path: "/dev/mmcblk0".to_string(),
            card_type: "eMMC".to_string(),
            model: "DG4064".to_string(),
            manufacturer: "0x45".to_string(),
            serial: "0x12345678".to_string(),
            firmware: "0x00".to_string(),
            total_bytes: 64000000000,
            health: health.clone(),
            telemetry: TelemetryStatus::default(),
            health_state: HealthState::default(),
        };

        let mmc_status = MmcStatus {
            initialized: true,
            devices: vec![device.clone()],
            message: None,
        };

        let nvme_status = NvmeStatus {
            initialized: true,
            devices: vec![],
            message: None,
        };

        let storage_status = StorageStatus {
            nvme: nvme_status.clone(),
            mmc: mmc_status.clone(),
        };

        let json = serde_json::to_string(&storage_status).expect("serialize storage_status");
        assert!(json.contains("\"lifeTimeEstAPercent\":10"));
        assert!(json.contains("\"blockPath\":\"/dev/mmcblk0\""));
        assert!(json.contains("\"mmc\":{"));
        assert!(json.contains("\"nvme\":{"));

        let deserialized: StorageStatus =
            serde_json::from_str(&json).expect("deserialize storage_status");
        assert_eq!(deserialized, storage_status);
    }
}
