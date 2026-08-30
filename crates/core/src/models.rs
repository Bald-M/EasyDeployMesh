use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootMode {
    Uefi,
    LegacyBios,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub id: String,
    pub model: String,
    pub serial: Option<String>,
    pub size_bytes: u64,
    pub is_system: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryModule {
    pub manufacturer: Option<String>,
    pub part_number: Option<String>,
    pub capacity_bytes: u64,
    pub speed_mhz: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareComponent {
    pub name: String,
    pub manufacturer: Option<String>,
    pub memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkAdapter {
    pub name: String,
    pub manufacturer: Option<String>,
    pub mac_address: Option<String>,
    pub speed_bps: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemDetails {
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub uptime_seconds: Option<u64>,
    pub motherboard: Option<String>,
    #[serde(default)]
    pub memory_modules: Vec<MemoryModule>,
    #[serde(default)]
    pub gpus: Vec<HardwareComponent>,
    #[serde(default)]
    pub displays: Vec<HardwareComponent>,
    #[serde(default)]
    pub audio_devices: Vec<HardwareComponent>,
    #[serde(default)]
    pub network_adapters: Vec<NetworkAdapter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: Uuid,
    pub hostname: Option<String>,
    pub mac_address: String,
    pub ip_address: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    #[serde(default)]
    pub cpu_model: Option<String>,
    #[serde(default)]
    pub physical_core_count: Option<u32>,
    #[serde(default)]
    pub logical_processor_count: u32,
    #[serde(default)]
    pub memory_bytes: u64,
    #[serde(default)]
    pub system_details: SystemDetails,
    pub architecture: Architecture,
    pub boot_mode: BootMode,
    pub disks: Vec<Disk>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInventory {
    pub hostname: Option<String>,
    pub mac_address: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    #[serde(default)]
    pub cpu_model: Option<String>,
    #[serde(default)]
    pub physical_core_count: Option<u32>,
    #[serde(default)]
    pub logical_processor_count: u32,
    #[serde(default)]
    pub memory_bytes: u64,
    #[serde(default)]
    pub system_details: SystemDetails,
    pub architecture: Architecture,
    pub boot_mode: BootMode,
    pub disks: Vec<Disk>,
    pub agent_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredDevice {
    pub device: Device,
    pub agent_version: String,
    pub first_seen_at: DateTime<Utc>,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRegistration {
    pub device_id: Uuid,
    pub device_token: String,
    pub heartbeat_interval_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHeartbeat {
    pub inventory: AgentInventory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHeartbeatAck {
    pub accepted_at: DateTime<Utc>,
    pub next_heartbeat_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPlaneStatus {
    pub state: String,
    pub bind_address: Option<String>,
    pub port: Option<u16>,
    pub endpoint: Option<String>,
    pub enrollment_token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PxeMode {
    StandaloneDhcp,
    ProxyDhcp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PxeConfig {
    pub mode: PxeMode,
    pub bind_address: String,
    pub subnet_mask: String,
    pub pool_start: String,
    pub pool_end: String,
    pub lease_seconds: u32,
    pub gateway: Option<String>,
    pub dns_servers: Vec<String>,
    pub tftp_root: String,
    pub bios_boot_file: String,
    pub uefi_x64_boot_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PxeServiceStatus {
    pub state: String,
    pub mode: Option<PxeMode>,
    pub bind_address: Option<String>,
    pub dhcp_port: Option<u16>,
    pub proxy_dhcp_port: Option<u16>,
    pub tftp_port: Option<u16>,
    pub active_leases: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PxeClientStage {
    Discovered,
    Downloading,
    WaitingForAgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PxeDiscoveredClient {
    pub mac_address: String,
    pub ip_address: Option<String>,
    pub architecture: Architecture,
    pub stage: PxeClientStage,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PxeEvent {
    pub level: String,
    pub event: String,
    pub message: String,
    pub mac_address: Option<String>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivitySource {
    Service,
    Device,
    Deployment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivitySeverity {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySubject {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub source: ActivitySource,
    pub kind: String,
    pub severity: ActivitySeverity,
    pub subject: Option<ActivitySubject>,
    #[serde(default)]
    pub details: serde_json::Map<String, serde_json::Value>,
    pub raw_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    Gho,
    Wim,
    Esd,
    Swm,
    Iso,
}

pub const UBUNTU_AUTOINSTALL_PROFILE_VERSION: u32 = 1;
pub const UBUNTU_MINIMUM_DISK_BYTES: u64 = 25 * 1024 * 1024 * 1024;
pub const UBUNTU_MINIMUM_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallerDistribution {
    Ubuntu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallerProfile {
    UbuntuAutoinstall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallerBootAsset {
    /// Canonical path below the managed image object's directory.
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallerCapability {
    pub deployable: bool,
    pub distribution: InstallerDistribution,
    /// The supported release series, currently exactly `24.04`.
    pub release: String,
    pub architecture: Architecture,
    pub profile: InstallerProfile,
    pub profile_version: u32,
    pub kernel: InstallerBootAsset,
    pub initrd: InstallerBootAsset,
    pub minimum_memory_bytes: u64,
    pub minimum_disk_bytes: u64,
    pub blocked_reason: Option<String>,
}

impl InstallerCapability {
    /// Returns whether this capability matches the only Linux installer profile
    /// currently implemented by the host.
    pub fn is_supported_ubuntu_server_v1(&self) -> bool {
        self.deployable
            && self.blocked_reason.is_none()
            && self.distribution == InstallerDistribution::Ubuntu
            && self.release == "24.04"
            && self.architecture == Architecture::X86_64
            && self.profile == InstallerProfile::UbuntuAutoinstall
            && self.profile_version == UBUNTU_AUTOINSTALL_PROFILE_VERSION
            && self.minimum_memory_bytes >= UBUNTU_MINIMUM_MEMORY_BYTES
            && self.minimum_disk_bytes >= UBUNTU_MINIMUM_DISK_BYTES
            && installer_asset_metadata_is_valid(&self.kernel)
            && installer_asset_metadata_is_valid(&self.initrd)
    }
}

fn installer_asset_metadata_is_valid(asset: &InstallerBootAsset) -> bool {
    !asset.path.trim().is_empty()
        && asset.size_bytes > 0
        && asset.sha256.len() == 64
        && asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhoPartitionCapability {
    pub source_partition: u32,
    pub file_system: String,
    pub expanded_size_bytes: u64,
    pub expanded_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhoImageCapability {
    pub deployable: bool,
    pub compression: Option<String>,
    pub expanded_size_bytes: Option<u64>,
    pub expanded_sha256: Option<String>,
    pub partition_count: Option<u32>,
    pub source_partition: Option<u32>,
    #[serde(default)]
    pub partitions: Vec<GhoPartitionCapability>,
    pub parser_version: u32,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageArtifact {
    pub id: Uuid,
    pub name: String,
    pub format: ImageFormat,
    pub source_path: String,
    pub size_bytes: u64,
    pub sha256: Option<String>,
    pub spans: Vec<String>,
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gho_capability: Option<GhoImageCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer_capability: Option<InstallerCapability>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    DeployGho,
    CaptureGho,
    DeployWim,
    InstallLinux,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub service_state: String,
    pub version: String,
    pub platform: String,
    pub active_interface: Option<String>,
    pub connected_devices: u32,
    pub queued_jobs: u32,
}
