use crate::{BootMode, Disk, Operation};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;
use uuid::Uuid;

pub const MIB_BYTES: u64 = 1024 * 1024;
pub const MINIMUM_WINDOWS_MIB: u64 = 20 * 1024;
pub const MINIMUM_DATA_MIB: u64 = 1024;
pub const IMAGE_CACHE_HEADROOM_MIB: u64 = 512;
pub const PARTITION_ALIGNMENT_HEADROOM_MIB: u64 = 32;

/// Lifecycle for a deployment or capture job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Draft,
    Waiting,
    Running,
    Paused,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    /// Applies a guarded state transition.
    pub fn transition(self, next: Self) -> Result<Self, JobTransitionError> {
        let allowed = matches!(
            (self, next),
            (Self::Draft, Self::Waiting)
                | (Self::Draft, Self::Cancelled)
                | (Self::Waiting, Self::Running)
                | (Self::Waiting, Self::Cancelled)
                | (Self::Running, Self::Paused)
                | (Self::Running, Self::Succeeded)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Cancelled)
                | (Self::Paused, Self::Running)
                | (Self::Paused, Self::Succeeded)
                | (Self::Paused, Self::Failed)
                | (Self::Paused, Self::Cancelled)
                | (Self::Failed, Self::Waiting)
        );

        allowed.then_some(next).ok_or(JobTransitionError {
            from: self,
            to: next,
        })
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("job cannot transition from {from:?} to {to:?}")]
pub struct JobTransitionError {
    pub from: JobState,
    pub to: JobState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentTarget {
    pub device_id: Uuid,
    pub target_disk_id: String,
    #[serde(default)]
    pub target_disk_model: String,
    #[serde(default)]
    pub target_disk_serial: Option<String>,
    #[serde(default)]
    pub target_disk_size_bytes: u64,
}

impl DeploymentTarget {
    /// Confirms that the physical disk still has the fingerprint selected by the service.
    pub fn matches_disk(&self, disk: &Disk) -> bool {
        const CAPACITY_TOLERANCE_BYTES: u64 = 1024 * 1024;

        disk.id.eq_ignore_ascii_case(self.target_disk_id.trim())
            && disk
                .model
                .trim()
                .eq_ignore_ascii_case(self.target_disk_model.trim())
            && disk.size_bytes.abs_diff(self.target_disk_size_bytes) <= CAPACITY_TOLERANCE_BYTES
            && self.target_disk_serial.as_deref().is_none_or(|expected| {
                disk.serial
                    .as_deref()
                    .is_some_and(|actual| actual.trim().eq_ignore_ascii_case(expected.trim()))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionTable {
    Gpt,
    Mbr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionRole {
    Efi,
    Msr,
    System,
    Windows,
    Recovery,
    Data,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionFileSystem {
    Fat32,
    Ntfs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionSpec {
    pub role: PartitionRole,
    /// A missing size means that this partition consumes the remaining space.
    pub size_mib: Option<u64>,
    pub file_system: Option<PartitionFileSystem>,
    pub label: String,
    #[serde(default)]
    pub drive_letter: Option<char>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionPlan {
    pub table: PartitionTable,
    pub partitions: Vec<PartitionSpec>,
}

impl PartitionPlan {
    pub fn recommended(boot_mode: BootMode) -> Option<Self> {
        match boot_mode {
            BootMode::Uefi => Some(Self::uefi_gpt()),
            BootMode::LegacyBios => Some(Self::legacy_bios_mbr()),
            BootMode::Unknown => None,
        }
    }

    pub fn uefi_gpt() -> Self {
        Self {
            table: PartitionTable::Gpt,
            partitions: vec![
                PartitionSpec {
                    role: PartitionRole::Efi,
                    size_mib: Some(300),
                    file_system: Some(PartitionFileSystem::Fat32),
                    label: "System".to_owned(),
                    drive_letter: None,
                },
                PartitionSpec {
                    role: PartitionRole::Msr,
                    size_mib: Some(16),
                    file_system: None,
                    label: String::new(),
                    drive_letter: None,
                },
                PartitionSpec {
                    role: PartitionRole::Windows,
                    size_mib: None,
                    file_system: Some(PartitionFileSystem::Ntfs),
                    label: "Windows".to_owned(),
                    drive_letter: None,
                },
            ],
        }
    }

    pub fn legacy_bios_mbr() -> Self {
        Self {
            table: PartitionTable::Mbr,
            partitions: vec![
                PartitionSpec {
                    role: PartitionRole::System,
                    size_mib: Some(550),
                    file_system: Some(PartitionFileSystem::Ntfs),
                    label: "System Reserved".to_owned(),
                    drive_letter: None,
                },
                PartitionSpec {
                    role: PartitionRole::Windows,
                    size_mib: None,
                    file_system: Some(PartitionFileSystem::Ntfs),
                    label: "Windows".to_owned(),
                    drive_letter: None,
                },
            ],
        }
    }

    pub fn validate(&self) -> Result<(), PartitionPlanError> {
        if self.partitions.is_empty() {
            return Err(PartitionPlanError::Empty);
        }
        let windows = self
            .partitions
            .iter()
            .filter(|partition| partition.role == PartitionRole::Windows)
            .count();
        if windows != 1 {
            return Err(PartitionPlanError::WindowsPartitionCount(windows));
        }
        let remaining = self
            .partitions
            .iter()
            .filter(|partition| partition.size_mib.is_none())
            .collect::<Vec<_>>();
        if remaining.len() != 1
            || !matches!(
                remaining[0].role,
                PartitionRole::Windows | PartitionRole::Data
            )
        {
            return Err(PartitionPlanError::InvalidRemainingPartition);
        }
        for partition in &self.partitions {
            if partition.size_mib == Some(0) {
                return Err(PartitionPlanError::ZeroSize(partition.role));
            }
            if partition.role == PartitionRole::Data
                && partition.file_system != Some(PartitionFileSystem::Ntfs)
            {
                return Err(PartitionPlanError::InvalidFileSystem(PartitionRole::Data));
            }
            if partition.label.len() > 32
                || partition.label.chars().any(|character| {
                    !character.is_ascii_alphanumeric() && !" _-".contains(character)
                })
            {
                return Err(PartitionPlanError::InvalidLabel(partition.label.clone()));
            }
            if let Some(letter) = partition.drive_letter {
                let letter = letter.to_ascii_uppercase();
                if partition.role != PartitionRole::Data
                    || !(('D'..='Q').contains(&letter)
                        || ('S'..='V').contains(&letter)
                        || ('X'..='Z').contains(&letter))
                {
                    return Err(PartitionPlanError::InvalidDriveLetter(letter));
                }
            }
        }
        let mut drive_letters = self
            .partitions
            .iter()
            .filter_map(|partition| {
                partition
                    .drive_letter
                    .map(|letter| letter.to_ascii_uppercase())
            })
            .collect::<Vec<_>>();
        drive_letters.sort_unstable();
        if drive_letters
            .windows(2)
            .any(|letters| letters[0] == letters[1])
        {
            return Err(PartitionPlanError::DuplicateDriveLetter);
        }

        match self.table {
            PartitionTable::Gpt => {
                require_partition(
                    &self.partitions,
                    PartitionRole::Efi,
                    Some(PartitionFileSystem::Fat32),
                )?;
                require_partition(&self.partitions, PartitionRole::Msr, None)?;
                if self
                    .partitions
                    .iter()
                    .any(|partition| partition.role == PartitionRole::System)
                {
                    return Err(PartitionPlanError::RoleDoesNotMatchTable(
                        PartitionRole::System,
                        PartitionTable::Gpt,
                    ));
                }
            }
            PartitionTable::Mbr => {
                require_partition(
                    &self.partitions,
                    PartitionRole::System,
                    Some(PartitionFileSystem::Ntfs),
                )?;
                if self.partitions.iter().any(|partition| {
                    matches!(partition.role, PartitionRole::Efi | PartitionRole::Msr)
                }) {
                    return Err(PartitionPlanError::RoleDoesNotMatchTable(
                        PartitionRole::Efi,
                        PartitionTable::Mbr,
                    ));
                }
            }
        }
        Ok(())
    }

    /// Validates that the physical disk can hold the final plan and the temporary image cache.
    pub fn validate_capacity(
        &self,
        disk_size_bytes: u64,
        image_size_bytes: u64,
    ) -> Result<(), PartitionCapacityError> {
        let available_mib = disk_size_bytes / MIB_BYTES;
        let fixed_mib = self
            .partitions
            .iter()
            .filter_map(|partition| partition.size_mib)
            .fold(0_u64, u64::saturating_add);
        let cache_mib = image_size_bytes
            .div_ceil(MIB_BYTES)
            .saturating_add(IMAGE_CACHE_HEADROOM_MIB);
        let remaining_minimum_mib = self
            .partitions
            .iter()
            .find(|partition| partition.size_mib.is_none())
            .map_or(MINIMUM_WINDOWS_MIB, |partition| {
                if partition.role == PartitionRole::Data {
                    MINIMUM_DATA_MIB
                } else {
                    MINIMUM_WINDOWS_MIB
                }
            });
        let required_mib = fixed_mib
            .saturating_add(cache_mib)
            .saturating_add(remaining_minimum_mib)
            .saturating_add(PARTITION_ALIGNMENT_HEADROOM_MIB);

        if available_mib < required_mib {
            return Err(PartitionCapacityError {
                required_mib,
                available_mib,
                fixed_mib,
                cache_mib,
                remaining_minimum_mib,
                alignment_headroom_mib: PARTITION_ALIGNMENT_HEADROOM_MIB,
            });
        }
        Ok(())
    }

    /// Ensures a raw partition payload fits in the Windows volume before any
    /// destructive partitioning begins.
    pub fn validate_windows_payload_capacity(
        &self,
        disk_size_bytes: u64,
        image_size_bytes: u64,
        payload_size_bytes: u64,
    ) -> Result<(), WindowsPayloadCapacityError> {
        let windows = self
            .partitions
            .iter()
            .find(|partition| partition.role == PartitionRole::Windows)
            .ok_or(WindowsPayloadCapacityError {
                required_bytes: payload_size_bytes,
                available_bytes: 0,
            })?;
        let available_bytes = if let Some(size_mib) = windows.size_mib {
            size_mib.saturating_mul(MIB_BYTES)
        } else {
            let fixed_mib = self
                .partitions
                .iter()
                .filter_map(|partition| partition.size_mib)
                .fold(0_u64, u64::saturating_add);
            let cache_mib = image_size_bytes
                .div_ceil(MIB_BYTES)
                .saturating_add(IMAGE_CACHE_HEADROOM_MIB);
            (disk_size_bytes / MIB_BYTES)
                .saturating_sub(fixed_mib)
                .saturating_sub(cache_mib)
                .saturating_sub(PARTITION_ALIGNMENT_HEADROOM_MIB)
                .saturating_mul(MIB_BYTES)
        };
        if available_bytes < payload_size_bytes {
            return Err(WindowsPayloadCapacityError {
                required_bytes: payload_size_bytes,
                available_bytes,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "raw Windows partition payload requires {required_bytes} bytes but the planned Windows volume provides {available_bytes} bytes"
)]
pub struct WindowsPayloadCapacityError {
    pub required_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "partition plan requires {required_mib} MiB but target disk provides {available_mib} MiB (fixed partitions: {fixed_mib} MiB, image cache: {cache_mib} MiB, remaining partition minimum: {remaining_minimum_mib} MiB, alignment reserve: {alignment_headroom_mib} MiB)"
)]
pub struct PartitionCapacityError {
    pub required_mib: u64,
    pub available_mib: u64,
    pub fixed_mib: u64,
    pub cache_mib: u64,
    pub remaining_minimum_mib: u64,
    pub alignment_headroom_mib: u64,
}

fn require_partition(
    partitions: &[PartitionSpec],
    role: PartitionRole,
    file_system: Option<PartitionFileSystem>,
) -> Result<(), PartitionPlanError> {
    let matches = partitions
        .iter()
        .filter(|partition| partition.role == role)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(PartitionPlanError::RequiredRoleCount(role, matches.len()));
    }
    if matches[0].file_system != file_system {
        return Err(PartitionPlanError::InvalidFileSystem(role));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PartitionPlanError {
    #[error("partition plan is empty")]
    Empty,
    #[error("partition plan requires exactly one Windows partition, found {0}")]
    WindowsPartitionCount(usize),
    #[error("exactly one Windows or data partition must consume remaining space")]
    InvalidRemainingPartition,
    #[error("partition {0:?} has a zero size")]
    ZeroSize(PartitionRole),
    #[error("partition label is invalid: {0}")]
    InvalidLabel(String),
    #[error("partition drive letter is invalid or reserved: {0}")]
    InvalidDriveLetter(char),
    #[error("partition drive letters must be unique")]
    DuplicateDriveLetter,
    #[error("partition plan requires exactly one {0:?} partition, found {1}")]
    RequiredRoleCount(PartitionRole, usize),
    #[error("partition {0:?} has an invalid file system")]
    InvalidFileSystem(PartitionRole),
    #[error("partition role {0:?} is incompatible with {1:?}")]
    RoleDoesNotMatchTable(PartitionRole, PartitionTable),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentOptions {
    pub image_index: u32,
    pub partition_plan: PartitionPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_install: Option<LinuxInstallOptions>,
}

impl Default for DeploymentOptions {
    fn default() -> Self {
        Self {
            image_index: 1,
            partition_plan: PartitionPlan::legacy_bios_mbr(),
            linux_install: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxInstallOptions {
    pub hostname: String,
    pub username: String,
    #[serde(default)]
    pub ssh_authorized_keys: Vec<String>,
}

impl LinuxInstallOptions {
    pub fn validate(&self) -> Result<(), LinuxInstallOptionsError> {
        if !valid_linux_hostname(&self.hostname) {
            return Err(LinuxInstallOptionsError::InvalidHostname);
        }
        if !valid_linux_username(&self.username) {
            return Err(LinuxInstallOptionsError::InvalidUsername);
        }
        if self.ssh_authorized_keys.is_empty() || self.ssh_authorized_keys.len() > 16 {
            return Err(LinuxInstallOptionsError::InvalidSshKeyCount);
        }
        let mut distinct_keys = HashSet::with_capacity(self.ssh_authorized_keys.len());
        for (index, key) in self.ssh_authorized_keys.iter().enumerate() {
            if !valid_ssh_public_key(key) {
                return Err(LinuxInstallOptionsError::InvalidSshKey { index });
            }
            if !distinct_keys.insert(key) {
                return Err(LinuxInstallOptionsError::DuplicateSshKey { index });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LinuxInstallOptionsError {
    #[error("Linux hostname must be a lowercase RFC 1123 label between 1 and 63 bytes")]
    InvalidHostname,
    #[error("Linux username must be a non-root lowercase account name between 1 and 32 bytes")]
    InvalidUsername,
    #[error("Linux installation requires between 1 and 16 SSH public keys")]
    InvalidSshKeyCount,
    #[error("SSH public key at index {index} is invalid or unsupported")]
    InvalidSshKey { index: usize },
    #[error("SSH public key at index {index} is duplicated")]
    DuplicateSshKey { index: usize },
}

fn valid_linux_hostname(hostname: &str) -> bool {
    let bytes = hostname.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn valid_linux_username(username: &str) -> bool {
    let bytes = username.as_bytes();
    (1..=32).contains(&bytes.len())
        && username != "root"
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'_' | b'-')
        })
}

fn valid_ssh_public_key(key: &str) -> bool {
    if key.is_empty()
        || key.len() > 16 * 1024
        || !key.is_ascii()
        || key
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
    {
        return false;
    }
    let mut fields = key.split_ascii_whitespace();
    let Some(algorithm) = fields.next() else {
        return false;
    };
    let Some(encoded) = fields.next() else {
        return false;
    };
    let supported_algorithm = matches!(
        algorithm,
        "ssh-ed25519"
            | "ssh-rsa"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
            | "sk-ssh-ed25519@openssh.com"
            | "sk-ecdsa-sha2-nistp256@openssh.com"
    );
    supported_algorithm
        && encoded.len() >= 16
        && encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentJob {
    pub id: Uuid,
    pub name: String,
    pub operation: Operation,
    pub image_id: Uuid,
    pub targets: Vec<DeploymentTarget>,
    #[serde(default)]
    pub options: DeploymentOptions,
    pub state: JobState,
    #[serde(default)]
    pub stage: Option<DeploymentStage>,
    pub progress_percent: u8,
    #[serde(default)]
    pub status_message: Option<String>,
    pub error_message: Option<String>,
    #[serde(default)]
    pub lease_id: Option<Uuid>,
    #[serde(default)]
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDeploymentJob {
    pub name: String,
    pub operation: Operation,
    pub image_id: Uuid,
    pub targets: Vec<DeploymentTarget>,
    #[serde(default)]
    pub options: DeploymentOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStage {
    Preflight,
    BootingInstaller,
    ValidatingTarget,
    Partitioning,
    DownloadingImage,
    ApplyingImage,
    InstallingSystem,
    ConfiguringBoot,
    Finalizing,
    AwaitingFirstBoot,
    Rebooting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxInstallerObservedDisk {
    pub path: String,
    pub model: String,
    pub serial: Option<String>,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxInstallerGuardRequest {
    pub image_sha256: String,
    pub disks: Vec<LinuxInstallerObservedDisk>,
}

impl LinuxInstallerGuardRequest {
    pub fn validate(&self) -> Result<(), LinuxInstallerGuardError> {
        if self.image_sha256.len() != 64
            || !self
                .image_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(LinuxInstallerGuardError::InvalidImageChecksum);
        }
        if self.disks.is_empty() || self.disks.len() > 128 {
            return Err(LinuxInstallerGuardError::InvalidDiskCount);
        }
        let mut paths = HashSet::with_capacity(self.disks.len());
        for (index, disk) in self.disks.iter().enumerate() {
            if !valid_observed_disk(disk) {
                return Err(LinuxInstallerGuardError::InvalidDisk { index });
            }
            if !paths.insert(disk.path.as_str()) {
                return Err(LinuxInstallerGuardError::DuplicateDiskPath { index });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LinuxInstallerGuardError {
    #[error("installer ISO SHA-256 must contain exactly 64 hexadecimal characters")]
    InvalidImageChecksum,
    #[error("installer inventory must contain between 1 and 128 disks")]
    InvalidDiskCount,
    #[error("installer disk at index {index} has invalid or unbounded metadata")]
    InvalidDisk { index: usize },
    #[error("installer disk at index {index} repeats an earlier device path")]
    DuplicateDiskPath { index: usize },
}

fn valid_observed_disk(disk: &LinuxInstallerObservedDisk) -> bool {
    let valid_text = |value: &str, max: usize| {
        !value.trim().is_empty() && value.len() <= max && !value.contains(['\r', '\n', '\0'])
    };
    let device_basename = disk.path.strip_prefix("/dev/");
    let valid_device_path = device_basename.is_some_and(|basename| {
        (1..=128).contains(&basename.len())
            && !matches!(basename, "." | "..")
            && basename
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    });
    valid_device_path
        && valid_text(&disk.path, 256)
        && valid_text(&disk.model, 256)
        && disk
            .serial
            .as_deref()
            .is_none_or(|serial| valid_text(serial, 256))
        && disk.size_bytes > 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDeploymentImage {
    pub id: Uuid,
    pub name: String,
    pub format: crate::ImageFormat,
    pub size_bytes: u64,
    pub sha256: String,
    pub download_url: String,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentGhoDeployment {
    pub source_partition: u32,
    pub expanded_size_bytes: u64,
    pub expanded_sha256: String,
    pub parser_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentJobLease {
    pub job_id: Uuid,
    pub lease_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub operation: Operation,
    pub image: AgentDeploymentImage,
    pub target: DeploymentTarget,
    pub partition_plan: PartitionPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gho: Option<AgentGhoDeployment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentJobProgress {
    pub lease_id: Uuid,
    pub stage: DeploymentStage,
    pub progress_percent: u8,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentJobCompletion {
    pub lease_id: Uuid,
    pub succeeded: bool,
    pub error_message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_reaches_succeeded() {
        let state = JobState::Draft
            .transition(JobState::Waiting)
            .unwrap()
            .transition(JobState::Running)
            .unwrap()
            .transition(JobState::Succeeded)
            .unwrap();

        assert_eq!(state, JobState::Succeeded);
        assert!(state.is_terminal());
    }

    #[test]
    fn failed_job_can_be_queued_for_retry() {
        assert_eq!(
            JobState::Failed.transition(JobState::Waiting),
            Ok(JobState::Waiting)
        );
    }

    #[test]
    fn running_job_can_pause_resume_and_finish() {
        let state = JobState::Running
            .transition(JobState::Paused)
            .expect("running job should pause")
            .transition(JobState::Running)
            .expect("paused job should resume")
            .transition(JobState::Succeeded)
            .expect("resumed job should finish");
        assert_eq!(state, JobState::Succeeded);
    }

    #[test]
    fn destructive_job_cannot_skip_waiting() {
        assert_eq!(
            JobState::Draft.transition(JobState::Running),
            Err(JobTransitionError {
                from: JobState::Draft,
                to: JobState::Running,
            })
        );
    }

    #[test]
    fn recommended_partition_plans_are_valid() {
        assert!(PartitionPlan::uefi_gpt().validate().is_ok());
        assert!(PartitionPlan::legacy_bios_mbr().validate().is_ok());
    }

    #[test]
    fn recommended_partition_plan_requires_a_known_boot_mode() {
        assert_eq!(PartitionPlan::recommended(BootMode::Unknown), None);
        assert_eq!(
            PartitionPlan::recommended(BootMode::Uefi),
            Some(PartitionPlan::uefi_gpt())
        );
        assert_eq!(
            PartitionPlan::recommended(BootMode::LegacyBios),
            Some(PartitionPlan::legacy_bios_mbr())
        );
    }

    #[test]
    fn deployment_target_matches_a_complete_disk_fingerprint() {
        let target = DeploymentTarget {
            device_id: Uuid::new_v4(),
            target_disk_id: r"\\.\PhysicalDrive0".to_owned(),
            target_disk_model: "Test SSD".to_owned(),
            target_disk_serial: Some("SERIAL-01".to_owned()),
            target_disk_size_bytes: 64 * 1024 * 1024 * 1024,
        };
        let disk = Disk {
            id: r"\\.\PHYSICALDRIVE0".to_owned(),
            model: " test ssd ".to_owned(),
            serial: Some("serial-01".to_owned()),
            size_bytes: target.target_disk_size_bytes,
            is_system: false,
        };

        assert!(target.matches_disk(&disk));
        let mut changed = disk;
        changed.serial = Some("OTHER".to_owned());
        assert!(!target.matches_disk(&changed));
    }

    #[test]
    fn remaining_space_can_be_assigned_to_a_data_partition() {
        let mut plan = PartitionPlan::legacy_bios_mbr();
        plan.partitions.last_mut().unwrap().size_mib = Some(30 * 1024);
        plan.partitions.push(PartitionSpec {
            role: PartitionRole::Data,
            size_mib: None,
            file_system: Some(PartitionFileSystem::Ntfs),
            label: "Data".to_owned(),
            drive_letter: Some('D'),
        });

        assert_eq!(plan.validate(), Ok(()));
    }

    #[test]
    fn partition_capacity_rejects_fixed_partitions_that_fill_the_target_disk() {
        let mut plan = PartitionPlan::uefi_gpt();
        plan.partitions.pop();
        plan.partitions.extend([
            PartitionSpec {
                role: PartitionRole::Windows,
                size_mib: Some(30 * 1024),
                file_system: Some(PartitionFileSystem::Ntfs),
                label: "Windows".to_owned(),
                drive_letter: None,
            },
            PartitionSpec {
                role: PartitionRole::Data,
                size_mib: Some(70 * 1024),
                file_system: Some(PartitionFileSystem::Ntfs),
                label: "Software".to_owned(),
                drive_letter: Some('D'),
            },
            PartitionSpec {
                role: PartitionRole::Data,
                size_mib: None,
                file_system: Some(PartitionFileSystem::Ntfs),
                label: "Data".to_owned(),
                drive_letter: Some('E'),
            },
        ]);

        let error = plan
            .validate_capacity(100_000_000_000, 5 * 1024 * 1024 * 1024)
            .expect_err("cache, alignment, and the remaining partition require extra capacity");
        assert!(error.required_mib > error.available_mib);
        assert_eq!(error.available_mib, 95_367);
    }

    #[test]
    fn raw_gho_payload_must_fit_the_planned_windows_volume() {
        let mut plan = PartitionPlan::uefi_gpt();
        let windows = plan
            .partitions
            .iter_mut()
            .find(|partition| partition.role == PartitionRole::Windows)
            .unwrap();
        windows.size_mib = Some(30 * 1024);
        plan.partitions.push(PartitionSpec {
            role: PartitionRole::Data,
            size_mib: None,
            file_system: Some(PartitionFileSystem::Ntfs),
            label: "Data".to_owned(),
            drive_letter: Some('D'),
        });

        assert!(
            plan.validate_windows_payload_capacity(
                128 * 1024 * MIB_BYTES,
                8 * 1024 * MIB_BYTES,
                31 * 1024 * MIB_BYTES,
            )
            .is_err()
        );
        assert!(
            plan.validate_windows_payload_capacity(
                128 * 1024 * MIB_BYTES,
                8 * 1024 * MIB_BYTES,
                30 * 1024 * MIB_BYTES,
            )
            .is_ok()
        );
    }

    #[test]
    fn old_deployment_options_deserialize_without_linux_fields() {
        let json = serde_json::json!({
            "imageIndex": 2,
            "partitionPlan": PartitionPlan::uefi_gpt()
        });

        let options: DeploymentOptions =
            serde_json::from_value(json).expect("legacy options should remain readable");

        assert_eq!(options.image_index, 2);
        assert_eq!(options.linux_install, None);
    }

    #[test]
    fn linux_install_options_require_safe_identity_and_ssh_keys() {
        let valid = LinuxInstallOptions {
            hostname: "worker-01".to_owned(),
            username: "deployer".to_owned(),
            ssh_authorized_keys: vec![
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEasyDeployMeshFixture deployer@example"
                    .to_owned(),
            ],
        };
        assert_eq!(valid.validate(), Ok(()));

        let mut invalid = valid.clone();
        invalid.hostname = "Worker_01".to_owned();
        assert_eq!(
            invalid.validate(),
            Err(LinuxInstallOptionsError::InvalidHostname)
        );

        let mut invalid = valid.clone();
        invalid.username = "root".to_owned();
        assert_eq!(
            invalid.validate(),
            Err(LinuxInstallOptionsError::InvalidUsername)
        );

        let mut invalid = valid;
        invalid.ssh_authorized_keys = vec!["not-a-public-key".to_owned()];
        assert_eq!(
            invalid.validate(),
            Err(LinuxInstallOptionsError::InvalidSshKey { index: 0 })
        );
    }

    #[test]
    fn installer_guard_request_bounds_untrusted_inventory() {
        let request = LinuxInstallerGuardRequest {
            image_sha256: "ab".repeat(32),
            disks: vec![LinuxInstallerObservedDisk {
                path: "/dev/nvme0n1".to_owned(),
                model: "Test NVMe".to_owned(),
                serial: Some("SERIAL-01".to_owned()),
                size_bytes: 64 * 1024 * 1024 * 1024,
            }],
        };
        assert_eq!(request.validate(), Ok(()));

        let mut duplicated = request;
        duplicated.disks.push(duplicated.disks[0].clone());
        assert_eq!(
            duplicated.validate(),
            Err(LinuxInstallerGuardError::DuplicateDiskPath { index: 1 })
        );

        duplicated.disks.truncate(1);
        duplicated.disks[0].path = "/dev/../../sda".to_owned();
        assert_eq!(
            duplicated.validate(),
            Err(LinuxInstallerGuardError::InvalidDisk { index: 0 })
        );

        duplicated.disks[0].path = "/dev/disk/by-id/example".to_owned();
        assert_eq!(
            duplicated.validate(),
            Err(LinuxInstallerGuardError::InvalidDisk { index: 0 })
        );
    }
}
