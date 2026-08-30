use easydeploymesh_core::{
    AgentHeartbeatAck, AgentInventory, AgentRegistration, Device, RegisteredDevice,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cmp::Reverse,
    fs, io,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::RwLock,
    time::Duration,
};
use subtle::ConstantTimeEq;
use thiserror::Error;
use uuid::Uuid;

const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_HEARTBEAT_INTERVAL_SECONDS: u64 = 10;
pub const DEVICE_ONLINE_WINDOW: Duration = Duration::from_secs(35);
pub const DEVICE_VERIFICATION_WINDOW: Duration = Duration::from_secs(12);

#[derive(Debug, Error)]
pub enum DeviceRegistryError {
    #[error("MAC address is invalid: {0}")]
    InvalidMacAddress(String),
    #[error("agent version is required")]
    MissingAgentVersion,
    #[error("device was not found: {0}")]
    NotFound(Uuid),
    #[error("device authentication failed")]
    Unauthorized,
    #[error("device registry lock was poisoned")]
    LockPoisoned,
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("device registry manifest is invalid: {0}")]
    InvalidManifest(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceRecord {
    device: Device,
    agent_version: String,
    first_seen_at: chrono::DateTime<chrono::Utc>,
    token_digest: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceManifest {
    schema_version: u32,
    devices: Vec<DeviceRecord>,
}

#[derive(Debug)]
pub struct DeviceRegistry {
    manifest_path: PathBuf,
    devices: RwLock<Vec<DeviceRecord>>,
}

impl DeviceRegistry {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, DeviceRegistryError> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir).map_err(|source| DeviceRegistryError::Write {
            path: data_dir.display().to_string(),
            source,
        })?;

        let manifest_path = data_dir.join("devices.json");
        let devices = if manifest_path.exists() {
            let bytes = fs::read(&manifest_path).map_err(|source| DeviceRegistryError::Read {
                path: manifest_path.display().to_string(),
                source,
            })?;
            let manifest: DeviceManifest = serde_json::from_slice(&bytes)?;
            manifest.devices
        } else {
            Vec::new()
        };

        Ok(Self {
            manifest_path,
            devices: RwLock::new(devices),
        })
    }

    pub fn list(&self) -> Result<Vec<RegisteredDevice>, DeviceRegistryError> {
        self.list_with_online_window(DEVICE_ONLINE_WINDOW)
    }

    pub fn list_with_online_window(
        &self,
        online_window: Duration,
    ) -> Result<Vec<RegisteredDevice>, DeviceRegistryError> {
        let now = chrono::Utc::now();
        let mut devices = self
            .devices
            .read()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?
            .iter()
            .map(|record| registered_device(record, now, online_window))
            .collect::<Vec<_>>();
        devices.sort_by_key(|entry| Reverse(entry.device.last_seen_at));
        Ok(devices)
    }

    /// Runs one decision against the authoritative inventory while preventing heartbeat updates.
    pub(crate) fn with_current<T>(
        &self,
        id: Uuid,
        inspect: impl FnOnce(&Device) -> T,
    ) -> Result<T, DeviceRegistryError> {
        let devices = self
            .devices
            .read()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        let current = devices
            .iter()
            .find(|record| record.device.id == id)
            .ok_or(DeviceRegistryError::NotFound(id))?;
        Ok(inspect(&current.device))
    }

    /// Resolves PXE identity through the same canonical MAC representation used at registration.
    pub(crate) fn find_by_mac(
        &self,
        mac_address: &str,
    ) -> Result<Option<Device>, DeviceRegistryError> {
        let mac_address = normalize_mac_address(mac_address)?;
        let devices = self
            .devices
            .read()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        Ok(devices
            .iter()
            .find(|record| record.device.mac_address == mac_address)
            .map(|record| record.device.clone()))
    }

    pub fn connected_count(&self) -> Result<u32, DeviceRegistryError> {
        let count = self.list()?.iter().filter(|device| device.online).count();
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    pub fn register(
        &self,
        mut inventory: AgentInventory,
        ip_address: IpAddr,
    ) -> Result<(RegisteredDevice, AgentRegistration), DeviceRegistryError> {
        validate_inventory(&mut inventory)?;
        let now = chrono::Utc::now();
        let device_token = generate_secret("easydeploymesh_device");
        let token_digest = digest_secret(&device_token);
        let mut devices = self
            .devices
            .write()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        let mut next_devices = devices.clone();

        let (id, first_seen_at) = next_devices
            .iter()
            .find(|record| record.device.mac_address == inventory.mac_address)
            .map_or_else(
                || (Uuid::new_v4(), now),
                |record| (record.device.id, record.first_seen_at),
            );
        let record = DeviceRecord {
            device: Device {
                id,
                hostname: inventory.hostname,
                mac_address: inventory.mac_address,
                ip_address: ip_address.to_string(),
                model: inventory.model,
                serial: inventory.serial,
                cpu_model: inventory.cpu_model,
                physical_core_count: inventory.physical_core_count,
                logical_processor_count: inventory.logical_processor_count,
                memory_bytes: inventory.memory_bytes,
                system_details: inventory.system_details,
                architecture: inventory.architecture,
                boot_mode: inventory.boot_mode,
                disks: inventory.disks,
                last_seen_at: now,
            },
            agent_version: inventory.agent_version,
            first_seen_at,
            token_digest,
        };

        next_devices.retain(|existing| existing.device.id != id);
        next_devices.push(record.clone());
        self.persist(&next_devices)?;
        *devices = next_devices;

        Ok((
            registered_device(&record, now, DEVICE_ONLINE_WINDOW),
            AgentRegistration {
                device_id: id,
                device_token,
                heartbeat_interval_seconds: DEFAULT_HEARTBEAT_INTERVAL_SECONDS,
            },
        ))
    }

    pub fn heartbeat(
        &self,
        id: Uuid,
        device_token: &str,
        mut inventory: AgentInventory,
        ip_address: IpAddr,
    ) -> Result<AgentHeartbeatAck, DeviceRegistryError> {
        validate_inventory(&mut inventory)?;
        let now = chrono::Utc::now();
        let mut devices = self
            .devices
            .write()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        let mut next_devices = devices.clone();
        let record = next_devices
            .iter_mut()
            .find(|record| record.device.id == id)
            .ok_or(DeviceRegistryError::NotFound(id))?;

        if !secret_matches(&record.token_digest, device_token) {
            return Err(DeviceRegistryError::Unauthorized);
        }

        record.device.hostname = inventory.hostname;
        record.device.mac_address = inventory.mac_address;
        record.device.ip_address = ip_address.to_string();
        record.device.model = inventory.model;
        record.device.serial = inventory.serial;
        record.device.cpu_model = inventory.cpu_model;
        record.device.physical_core_count = inventory.physical_core_count;
        record.device.logical_processor_count = inventory.logical_processor_count;
        record.device.memory_bytes = inventory.memory_bytes;
        record.device.system_details = inventory.system_details;
        record.device.architecture = inventory.architecture;
        record.device.boot_mode = inventory.boot_mode;
        record.device.disks = inventory.disks;
        record.device.last_seen_at = now;
        record.agent_version = inventory.agent_version;

        self.persist(&next_devices)?;
        *devices = next_devices;

        Ok(AgentHeartbeatAck {
            accepted_at: now,
            next_heartbeat_seconds: DEFAULT_HEARTBEAT_INTERVAL_SECONDS,
        })
    }

    pub fn authenticate(&self, id: Uuid, device_token: &str) -> Result<(), DeviceRegistryError> {
        let devices = self
            .devices
            .read()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        let record = devices
            .iter()
            .find(|record| record.device.id == id)
            .ok_or(DeviceRegistryError::NotFound(id))?;
        if !secret_matches(&record.token_digest, device_token) {
            return Err(DeviceRegistryError::Unauthorized);
        }
        Ok(())
    }

    pub fn remove(&self, id: Uuid) -> Result<bool, DeviceRegistryError> {
        let mut devices = self
            .devices
            .write()
            .map_err(|_| DeviceRegistryError::LockPoisoned)?;
        let mut next_devices = devices.clone();
        next_devices.retain(|record| record.device.id != id);
        if next_devices.len() == devices.len() {
            return Ok(false);
        }

        self.persist(&next_devices)?;
        *devices = next_devices;
        Ok(true)
    }

    fn persist(&self, devices: &[DeviceRecord]) -> Result<(), DeviceRegistryError> {
        let manifest = DeviceManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            devices: devices.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&manifest)?;
        fs::write(&self.manifest_path, bytes).map_err(|source| DeviceRegistryError::Write {
            path: self.manifest_path.display().to_string(),
            source,
        })
    }
}

fn validate_inventory(inventory: &mut AgentInventory) -> Result<(), DeviceRegistryError> {
    inventory.mac_address = normalize_mac_address(&inventory.mac_address)?;
    inventory.hostname = inventory
        .hostname
        .take()
        .map(|hostname| hostname.trim().chars().take(255).collect())
        .filter(|hostname: &String| !hostname.is_empty());
    inventory.model = inventory
        .model
        .take()
        .map(|model| model.trim().chars().take(255).collect())
        .filter(|model: &String| !model.is_empty());
    inventory.serial = inventory
        .serial
        .take()
        .map(|serial| serial.trim().chars().take(255).collect())
        .filter(|serial: &String| !serial.is_empty());
    normalize_optional_text(&mut inventory.cpu_model);
    normalize_optional_text(&mut inventory.system_details.os_name);
    normalize_optional_text(&mut inventory.system_details.os_version);
    normalize_optional_text(&mut inventory.system_details.motherboard);
    inventory.system_details.memory_modules.truncate(64);
    for module in &mut inventory.system_details.memory_modules {
        normalize_optional_text(&mut module.manufacturer);
        normalize_optional_text(&mut module.part_number);
    }
    inventory.system_details.gpus.truncate(32);
    inventory.system_details.displays.truncate(32);
    inventory.system_details.audio_devices.truncate(32);
    for component in inventory
        .system_details
        .gpus
        .iter_mut()
        .chain(inventory.system_details.displays.iter_mut())
        .chain(inventory.system_details.audio_devices.iter_mut())
    {
        component.name = normalized_text(&component.name);
        normalize_optional_text(&mut component.manufacturer);
    }
    inventory
        .system_details
        .gpus
        .retain(|component| !component.name.is_empty());
    inventory
        .system_details
        .displays
        .retain(|component| !component.name.is_empty());
    inventory
        .system_details
        .audio_devices
        .retain(|component| !component.name.is_empty());
    inventory.system_details.network_adapters.truncate(64);
    for adapter in &mut inventory.system_details.network_adapters {
        adapter.name = normalized_text(&adapter.name);
        normalize_optional_text(&mut adapter.manufacturer);
        normalize_optional_text(&mut adapter.mac_address);
    }
    inventory
        .system_details
        .network_adapters
        .retain(|adapter| !adapter.name.is_empty());
    inventory.agent_version = inventory.agent_version.trim().chars().take(64).collect();
    if inventory.agent_version.is_empty() {
        return Err(DeviceRegistryError::MissingAgentVersion);
    }
    Ok(())
}

fn normalized_text(value: &str) -> String {
    value.trim().chars().take(255).collect()
}

fn normalize_optional_text(value: &mut Option<String>) {
    *value = value
        .take()
        .map(|value| normalized_text(&value))
        .filter(|value| !value.is_empty());
}

fn normalize_mac_address(value: &str) -> Result<String, DeviceRegistryError> {
    let hexadecimal = value
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .map(|character| character.to_ascii_uppercase())
        .collect::<String>();
    if hexadecimal.len() != 12 {
        return Err(DeviceRegistryError::InvalidMacAddress(value.to_owned()));
    }

    Ok(hexadecimal
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).expect("ASCII hexadecimal must be UTF-8"))
        .collect::<Vec<_>>()
        .join(":"))
}

fn registered_device(
    record: &DeviceRecord,
    now: chrono::DateTime<chrono::Utc>,
    online_window: Duration,
) -> RegisteredDevice {
    let age = now
        .signed_duration_since(record.device.last_seen_at)
        .to_std()
        .unwrap_or_default();
    RegisteredDevice {
        device: record.device.clone(),
        agent_version: record.agent_version.clone(),
        first_seen_at: record.first_seen_at,
        online: age <= online_window,
    }
}

pub(crate) fn generate_secret(prefix: &str) -> String {
    format!(
        "{prefix}_{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

pub(crate) fn digest_secret(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

pub(crate) fn secret_matches(expected_digest: &str, candidate: &str) -> bool {
    let candidate_digest = digest_secret(candidate);
    expected_digest.len() == candidate_digest.len()
        && bool::from(
            expected_digest
                .as_bytes()
                .ct_eq(candidate_digest.as_bytes()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use easydeploymesh_core::{Architecture, BootMode};

    fn inventory(mac_address: &str) -> AgentInventory {
        AgentInventory {
            hostname: Some("lab-pc-01".to_owned()),
            mac_address: mac_address.to_owned(),
            model: Some("Test workstation".to_owned()),
            serial: None,
            cpu_model: Some("Test CPU".to_owned()),
            physical_core_count: Some(4),
            logical_processor_count: 8,
            memory_bytes: 8 * 1024 * 1024 * 1024,
            system_details: Default::default(),
            architecture: Architecture::X86_64,
            boot_mode: BootMode::Uefi,
            disks: Vec::new(),
            agent_version: "0.1.0".to_owned(),
        }
    }

    #[test]
    fn registration_is_persistent_and_normalizes_mac_address() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = DeviceRegistry::open(temp.path()).expect("registry should open");
        let (device, registration) = registry
            .register(
                inventory("aa-bb-cc-dd-ee-ff"),
                "192.168.10.20".parse().expect("IP should parse"),
            )
            .expect("device should register");

        assert_eq!(device.device.mac_address, "AA:BB:CC:DD:EE:FF");
        assert!(device.online);
        assert!(
            registration
                .device_token
                .starts_with("easydeploymesh_device_")
        );

        let reloaded = DeviceRegistry::open(temp.path()).expect("registry should reload");
        assert_eq!(
            reloaded.list().expect("devices should list")[0].device.id,
            device.device.id
        );
    }

    #[test]
    fn manual_verification_uses_one_heartbeat_cycle_instead_of_the_offline_window() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = DeviceRegistry::open(temp.path()).expect("registry should open");
        registry
            .register(
                inventory("AA:BB:CC:DD:EE:FF"),
                "192.168.10.20".parse().expect("IP should parse"),
            )
            .expect("device should register");

        registry
            .devices
            .write()
            .expect("device registry should lock")[0]
            .device
            .last_seen_at = chrono::Utc::now() - chrono::Duration::seconds(13);

        assert!(registry.list().expect("devices should list")[0].online);
        assert!(
            !registry
                .list_with_online_window(DEVICE_VERIFICATION_WINDOW)
                .expect("devices should verify")[0]
                .online
        );
    }

    #[test]
    fn heartbeat_requires_the_device_secret() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = DeviceRegistry::open(temp.path()).expect("registry should open");
        let (device, registration) = registry
            .register(
                inventory("AA:BB:CC:DD:EE:FF"),
                "192.168.10.20".parse().expect("IP should parse"),
            )
            .expect("device should register");

        assert!(matches!(
            registry.heartbeat(
                device.device.id,
                "wrong-token",
                inventory("AA:BB:CC:DD:EE:FF"),
                "192.168.10.21".parse().expect("IP should parse")
            ),
            Err(DeviceRegistryError::Unauthorized)
        ));

        let ack = registry
            .heartbeat(
                device.device.id,
                &registration.device_token,
                inventory("AA:BB:CC:DD:EE:FF"),
                "192.168.10.21".parse().expect("IP should parse"),
            )
            .expect("valid heartbeat should work");
        assert_eq!(
            ack.next_heartbeat_seconds,
            DEFAULT_HEARTBEAT_INTERVAL_SECONDS
        );
        assert_eq!(
            registry.list().expect("devices should list")[0]
                .device
                .ip_address,
            "192.168.10.21"
        );
    }

    #[test]
    fn re_registration_preserves_device_identity_and_rotates_secret() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = DeviceRegistry::open(temp.path()).expect("registry should open");
        let (first, first_registration) = registry
            .register(
                inventory("AA:BB:CC:DD:EE:FF"),
                "192.168.10.20".parse().expect("IP should parse"),
            )
            .expect("device should register");
        let (second, second_registration) = registry
            .register(
                inventory("aa:bb:cc:dd:ee:ff"),
                "192.168.10.30".parse().expect("IP should parse"),
            )
            .expect("device should re-register");

        assert_eq!(first.device.id, second.device.id);
        assert_ne!(
            first_registration.device_token,
            second_registration.device_token
        );
        assert_eq!(registry.list().expect("devices should list").len(), 1);
    }

    #[test]
    fn pxe_lookup_normalizes_mac_and_rejects_malformed_identity() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let registry = DeviceRegistry::open(temp.path()).expect("registry should open");
        let (registered, _) = registry
            .register(
                inventory("AA:BB:CC:DD:EE:FF"),
                "192.168.10.20".parse().expect("IP should parse"),
            )
            .expect("device should register");

        assert_eq!(
            registry
                .find_by_mac("aa-bb-cc-dd-ee-ff")
                .expect("lookup should succeed")
                .expect("device should exist")
                .id,
            registered.device.id
        );
        assert!(matches!(
            registry.find_by_mac("not-a-mac"),
            Err(DeviceRegistryError::InvalidMacAddress(_))
        ));
    }
}
