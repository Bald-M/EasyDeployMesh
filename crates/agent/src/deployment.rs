use easydeploymesh_bcd::LoaderIntegrityPolicy;
#[cfg(target_os = "windows")]
use easydeploymesh_core::WINPE_DRIVE_LETTER;
use easydeploymesh_core::{
    AgentJobLease, CACHE_DRIVE_LETTER, DeploymentStage, Disk, ImageFormat, JobState, MIB_BYTES,
    Operation, PARTITION_ALIGNMENT_HEADROOM_MIB, PartitionFileSystem, PartitionRole, PartitionSpec,
    PartitionTable, SYSTEM_DRIVE_LETTER, WINDOWS_DRIVE_LETTER,
};
use reqwest::blocking::Client;
#[cfg(target_os = "windows")]
use sha2::{Digest, Sha256};
use std::{error::Error, path::PathBuf};
#[cfg(target_os = "windows")]
use std::{ffi::c_void, os::windows::io::AsRawHandle};
#[cfg(target_os = "windows")]
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

#[cfg(target_os = "windows")]
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSuspendProcess(process_handle: *mut c_void) -> i32;
    fn NtResumeProcess(process_handle: *mut c_void) -> i32;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedLayout {
    prepare_script: String,
    cleanup_script: String,
    image_path: PathBuf,
    firmware: &'static str,
    disk_number: u32,
    windows_partition: u32,
    windows_partition_bytes: u64,
    windows_file_system: Option<PartitionFileSystem>,
}

/// Which local byte counter a [`LocalProgress::Transfer`] describes.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferPhase {
    Download,
    Verify,
    Restore,
}

/// Where the Windows partition will be written, for display only.
#[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetLayout {
    pub disk_number: u32,
    pub disk_model: String,
    pub windows_partition: u32,
    pub windows_partition_bytes: u64,
    pub file_system: Option<PartitionFileSystem>,
}

/// Progress that only drives the on-screen window. It is never sent to the
/// control service, so the reported stages and percentages stay unchanged.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalProgress {
    /// The validated partition layout, emitted before any disk is touched.
    Layout(TargetLayout),
    /// Emitted immediately before the first command that writes to the disk.
    DiskWriteStarting,
    Transfer {
        phase: TransferPhase,
        done: u64,
        total: u64,
    },
}

#[allow(clippy::too_many_arguments)]
pub fn execute(
    lease: &AgentJobLease,
    inventory_disks: &[Disk],
    server: &str,
    device_token: &str,
    client: &Client,
    mut progress: impl FnMut(DeploymentStage, u8, &str) -> Result<(), Box<dyn Error>>,
    #[allow(unused_mut)] mut control_state: impl FnMut() -> Result<JobState, Box<dyn Error>>,
    mut local: impl FnMut(LocalProgress),
) -> Result<(), Box<dyn Error>> {
    if !matches!(
        (lease.operation, lease.image.format),
        (Operation::DeployWim, ImageFormat::Wim | ImageFormat::Esd)
            | (Operation::DeployGho, ImageFormat::Gho)
    ) {
        return Err("deployment operation and image format are incompatible".into());
    }
    if lease.operation == Operation::DeployGho && lease.gho.is_none() {
        return Err("GHO deployment metadata is missing".into());
    }
    progress(
        DeploymentStage::Preflight,
        2,
        "Validating target disk fingerprint",
    )?;
    let disk = matching_disk(lease, inventory_disks)?;
    let layout = prepare_layout(lease, disk)?;
    local(LocalProgress::Layout(TargetLayout {
        disk_number: layout.disk_number,
        disk_model: disk.model.clone(),
        windows_partition: layout.windows_partition,
        windows_partition_bytes: layout.windows_partition_bytes,
        file_system: layout.windows_file_system,
    }));

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (server, device_token, client, layout, control_state);
        Err("deployment execution is available only inside Windows PE".into())
    }

    #[cfg(target_os = "windows")]
    {
        let work_directory = PathBuf::from(format!(r"{WINPE_DRIVE_LETTER}:\EasyDeployMesh\work"))
            .join(lease.job_id.to_string());
        fs::create_dir_all(&work_directory)?;
        let prepare_script = work_directory.join("partition.txt");
        let cleanup_script = work_directory.join("cleanup.txt");
        fs::write(&prepare_script, &layout.prepare_script)?;
        fs::write(&cleanup_script, &layout.cleanup_script)?;

        progress(
            DeploymentStage::Partitioning,
            10,
            "Partitioning the selected disk",
        )?;
        await_running(&mut control_state)?;
        local(LocalProgress::DiskWriteStarting);
        run_checked(
            "diskpart.exe",
            [OsStr::new("/s"), prepare_script.as_os_str()],
            &mut control_state,
        )?;

        progress(
            DeploymentStage::DownloadingImage,
            20,
            "Downloading the deployment image",
        )?;
        download_image(
            lease,
            server,
            device_token,
            client,
            &layout.image_path,
            &mut control_state,
            &mut local,
        )?;
        verify_image(
            &layout.image_path,
            &lease.image.sha256,
            &mut control_state,
            &mut local,
        )?;

        progress(
            DeploymentStage::ApplyingImage,
            45,
            "Applying the Windows image",
        )?;
        let mut image_boot_policy = LoaderIntegrityPolicy::default();
        if lease.operation == Operation::DeployGho {
            let gho = lease
                .gho
                .as_ref()
                .ok_or("GHO deployment metadata is missing")?;
            await_running(&mut control_state)?;
            let grow_file_system =
                restore_gho_partition(&layout.image_path, gho, &mut control_state, &mut local)?;
            // The restored volume may carry its own boot store from the
            // machine it was captured on; bcdboot below builds a new one.
            image_boot_policy = read_image_boot_policy(Path::new(r"W:\Boot\BCD"));
            if grow_file_system {
                // The restored NTFS keeps its original size; let Windows grow
                // it to fill the target partition.
                let extend_script = work_directory.join("extend-filesystem.txt");
                fs::write(&extend_script, "select volume W\nextend filesystem\nexit\n")?;
                await_running(&mut control_state)?;
                run_checked(
                    "diskpart.exe",
                    [OsStr::new("/s"), extend_script.as_os_str()],
                    &mut control_state,
                )?;
            }
        } else {
            let mut image_argument = OsString::from("/ImageFile:");
            image_argument.push(layout.image_path.as_os_str());
            await_running(&mut control_state)?;
            run_checked(
                "dism.exe",
                [
                    OsString::from("/English"),
                    OsString::from("/Apply-Image"),
                    image_argument,
                    OsString::from(format!("/Index:{}", lease.image.index)),
                    OsString::from(format!(r"/ApplyDir:{WINDOWS_DRIVE_LETTER}:\")),
                    OsString::from("/CheckIntegrity"),
                ],
                &mut control_state,
            )?;
        }

        progress(
            DeploymentStage::ConfiguringBoot,
            88,
            "Creating Windows boot files",
        )?;
        await_running(&mut control_state)?;
        run_checked(
            "bcdboot.exe",
            [
                OsString::from(format!(r"{WINDOWS_DRIVE_LETTER}:\Windows")),
                OsString::from("/s"),
                OsString::from(format!("{SYSTEM_DRIVE_LETTER}:")),
                OsString::from("/f"),
                OsString::from(layout.firmware),
                OsString::from("/c"),
            ],
            &mut control_state,
        )?;
        let policy_commands = boot_policy_commands(layout.firmware, image_boot_policy);
        if !policy_commands.is_empty() {
            progress(
                DeploymentStage::ConfiguringBoot,
                90,
                "Keeping the image's boot policy: boot integrity checks disabled",
            )?;
            for arguments in policy_commands {
                await_running(&mut control_state)?;
                run_checked("bcdedit.exe", arguments, &mut control_state)?;
            }
        }

        progress(
            DeploymentStage::Finalizing,
            95,
            "Removing the temporary image cache",
        )?;
        await_running(&mut control_state)?;
        run_checked(
            "diskpart.exe",
            [OsStr::new("/s"), cleanup_script.as_os_str()],
            &mut control_state,
        )?;
        progress(
            DeploymentStage::Rebooting,
            99,
            "Deployment complete; preparing to reboot",
        )?;
        Ok(())
    }
}

/// Reads the boot-integrity policy of a restored image's own boot store. A
/// missing or unreadable store means there is nothing to carry over.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn read_image_boot_policy(path: &std::path::Path) -> LoaderIntegrityPolicy {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| easydeploymesh_bcd::windows_loader_integrity_policy(&bytes).ok())
        .unwrap_or_default()
}

/// `bcdedit` arguments that re-apply the image's boot-integrity policy to the
/// store bcdboot created on the system partition. Some repackaged Windows 7
/// images ship a boot loader that the image's own Boot Manager only starts
/// with integrity checks disabled, and record that in their original store.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn boot_policy_commands(firmware: &str, policy: LoaderIntegrityPolicy) -> Vec<Vec<String>> {
    if !policy.disable_integrity_checks {
        return Vec::new();
    }
    let store = if firmware == "UEFI" {
        r"S:\EFI\Microsoft\Boot\BCD"
    } else {
        r"S:\Boot\BCD"
    };
    [
        ["loadoptions", "DISABLE_INTEGRITY_CHECKS"],
        ["nointegritychecks", "Yes"],
    ]
    .into_iter()
    .map(|[name, value]| {
        ["/store", store, "/set", "{default}", name, value]
            .into_iter()
            .map(str::to_owned)
            .collect()
    })
    .collect()
}

fn matching_disk<'a>(lease: &AgentJobLease, disks: &'a [Disk]) -> Result<&'a Disk, Box<dyn Error>> {
    let target = &lease.target;
    let disk = disks
        .iter()
        .find(|disk| disk.id.eq_ignore_ascii_case(&target.target_disk_id))
        .ok_or("the selected physical disk is no longer present")?;
    if !target.matches_disk(disk) {
        return Err("target disk fingerprint changed after the deployment was confirmed".into());
    }
    if !target.can_deploy_to(disk) {
        return Err(
            "the selected physical disk hosts the running system and cannot be deployed to".into(),
        );
    }
    Ok(disk)
}

fn prepare_layout(lease: &AgentJobLease, disk: &Disk) -> Result<PreparedLayout, Box<dyn Error>> {
    lease.partition_plan.validate()?;
    if lease.image.index == 0 {
        return Err("Windows image index must be greater than zero".into());
    }
    if lease
        .partition_plan
        .partitions
        .iter()
        .any(|partition| partition.role == PartitionRole::Recovery)
    {
        return Err("recovery partitions are not yet supported by the executor".into());
    }
    lease
        .partition_plan
        .validate_capacity(disk.size_bytes, lease.image.size_bytes)?;
    if let Some(gho) = &lease.gho {
        if gho.volume_size_bytes == 0 {
            return Err("GHO lease does not state the restored volume size".into());
        }
        lease.partition_plan.validate_windows_payload_capacity(
            disk.size_bytes,
            lease.image.size_bytes,
            gho.volume_size_bytes,
        )?;
    }

    let disk_number = physical_drive_number(&disk.id)?;
    let fixed_mib = lease
        .partition_plan
        .partitions
        .iter()
        .filter_map(|partition| partition.size_mib)
        .sum::<u64>();
    let cache_mib = lease
        .image
        .size_bytes
        .div_ceil(MIB_BYTES)
        .saturating_add(easydeploymesh_core::IMAGE_CACHE_HEADROOM_MIB);
    let disk_mib = disk.size_bytes / MIB_BYTES;
    let remaining_role = lease
        .partition_plan
        .partitions
        .iter()
        .find(|partition| partition.size_mib.is_none())
        .map(|partition| partition.role)
        .ok_or("partition plan has no remaining-space partition")?;
    let remaining_mib = disk_mib - fixed_mib - cache_mib - PARTITION_ALIGNMENT_HEADROOM_MIB;
    let data_partition_count = lease
        .partition_plan
        .partitions
        .iter()
        .filter(|partition| partition.role == PartitionRole::Data)
        .count();
    let use_mbr_extended_partition = lease.partition_plan.table == PartitionTable::Mbr
        && data_partition_count.saturating_add(3) > 4;
    if use_mbr_extended_partition {
        let first_data = lease
            .partition_plan
            .partitions
            .iter()
            .position(|partition| partition.role == PartitionRole::Data)
            .ok_or("MBR extended layout has no data partition")?;
        if lease.partition_plan.partitions[first_data..]
            .iter()
            .any(|partition| partition.role != PartitionRole::Data)
        {
            return Err(
                "MBR plans that require an extended partition must place data partitions last"
                    .into(),
            );
        }
    }
    let mut script = format!(
        "select disk {disk_number}\nclean\nconvert {}\n",
        match lease.partition_plan.table {
            PartitionTable::Gpt => "gpt",
            PartitionTable::Mbr => "mbr",
        }
    );
    let mut windows_partition = 0_u32;
    let mut windows_partition_bytes = 0_u64;
    let mut windows_file_system = None;
    let mut created_extended_partition = false;
    for (index, partition) in lease.partition_plan.partitions.iter().enumerate() {
        if partition.role == PartitionRole::Windows {
            windows_partition = u32::try_from(index + 1)?;
            windows_partition_bytes = partition
                .size_mib
                .unwrap_or(remaining_mib)
                .saturating_mul(MIB_BYTES);
            windows_file_system = partition.file_system;
        }
        let partition_kind = if use_mbr_extended_partition && partition.role == PartitionRole::Data
        {
            if !created_extended_partition {
                script.push_str("create partition extended\n");
                created_extended_partition = true;
            }
            "logical"
        } else {
            match partition.role {
                PartitionRole::Efi => "efi",
                PartitionRole::Msr => "msr",
                _ => "primary",
            }
        };
        if partition.size_mib.is_none() {
            script.push_str(&format!(
                "create partition {partition_kind} size={remaining_mib}\n"
            ));
        } else {
            let size = partition.size_mib.ok_or("partition size is missing")?;
            script.push_str(&format!("create partition {partition_kind} size={size}\n"));
        }
        if let Some(file_system) = partition.file_system {
            let file_system = match file_system {
                PartitionFileSystem::Fat32 => "fat32",
                PartitionFileSystem::Ntfs => "ntfs",
            };
            script.push_str(&format!(
                "format quick fs={file_system} label=\"{}\"\n",
                partition.label
            ));
        }
        match partition.role {
            PartitionRole::Efi => {
                script.push_str(&format!("assign letter={SYSTEM_DRIVE_LETTER}\n"));
            }
            PartitionRole::System => {
                if lease.partition_plan.table == PartitionTable::Mbr {
                    script.push_str("active\n");
                }
                script.push_str(&format!("assign letter={SYSTEM_DRIVE_LETTER}\n"));
            }
            PartitionRole::Windows => {
                script.push_str(&format!("assign letter={WINDOWS_DRIVE_LETTER}\n"));
            }
            PartitionRole::Data => {
                let letter = partition
                    .resolved_data_drive_letter()
                    .ok_or("data partition has no drive letter")?;
                script.push_str(&format!("assign letter={letter}\n"));
            }
            _ => {}
        }
    }
    let cache_partition_kind = if use_mbr_extended_partition {
        "logical"
    } else {
        "primary"
    };
    script.push_str(&format!(
        "create partition {cache_partition_kind} size={cache_mib}\nformat quick fs=ntfs label=\"EasyDeployMesh Cache\"\nassign letter={CACHE_DRIVE_LETTER}\nexit\n"
    ));
    let extend_letter = if remaining_role == PartitionRole::Data {
        lease
            .partition_plan
            .partitions
            .iter()
            .find(|partition| partition.size_mib.is_none())
            .and_then(PartitionSpec::resolved_data_drive_letter)
            .ok_or("remaining-space data partition has no drive letter")?
    } else {
        WINDOWS_DRIVE_LETTER
    };
    let cleanup_script = format!(
        "select disk {disk_number}\nselect volume {CACHE_DRIVE_LETTER}\ndelete volume\nselect volume {extend_letter}\nextend\nexit\n"
    );
    let extension = match lease.image.format {
        ImageFormat::Wim => "wim",
        ImageFormat::Esd => "esd",
        ImageFormat::Gho => "gho",
        _ => return Err("unsupported automated image format".into()),
    };
    Ok(PreparedLayout {
        prepare_script: script,
        cleanup_script,
        image_path: PathBuf::from(format!(
            r"{CACHE_DRIVE_LETTER}:\EasyDeployMesh\image.{extension}"
        )),
        firmware: match lease.partition_plan.table {
            PartitionTable::Gpt => "UEFI",
            PartitionTable::Mbr => "BIOS",
        },
        disk_number,
        windows_partition,
        windows_partition_bytes,
        windows_file_system,
    })
}

fn physical_drive_number(id: &str) -> Result<u32, Box<dyn Error>> {
    let digits = id
        .trim()
        .to_ascii_lowercase()
        .strip_prefix(r"\\.\physicaldrive")
        .ok_or("target disk id is not a PhysicalDrive path")?
        .to_owned();
    Ok(digits.parse()?)
}

/// Throttles local byte counters so the progress window repaints a few times a
/// second at most, while always reporting the first and final values.
#[cfg(target_os = "windows")]
struct TransferReporter<'a> {
    local: &'a mut dyn FnMut(LocalProgress),
    phase: TransferPhase,
    total: u64,
    last_report: Option<std::time::Instant>,
}

#[cfg(target_os = "windows")]
impl<'a> TransferReporter<'a> {
    const INTERVAL: Duration = Duration::from_millis(250);

    fn new(local: &'a mut dyn FnMut(LocalProgress), phase: TransferPhase, total: u64) -> Self {
        let mut reporter = Self {
            local,
            phase,
            total,
            last_report: None,
        };
        reporter.report(0, true);
        reporter
    }

    fn report(&mut self, done: u64, force: bool) {
        let now = std::time::Instant::now();
        if !force
            && self
                .last_report
                .is_some_and(|last| now.duration_since(last) < Self::INTERVAL)
        {
            return;
        }
        self.last_report = Some(now);
        (self.local)(LocalProgress::Transfer {
            phase: self.phase,
            done,
            total: self.total,
        });
    }
}

#[cfg(target_os = "windows")]
fn download_image(
    lease: &AgentJobLease,
    server: &str,
    device_token: &str,
    client: &Client,
    destination: &Path,
    control_state: &mut impl FnMut() -> Result<JobState, Box<dyn Error>>,
    local: &mut dyn FnMut(LocalProgress),
) -> Result<(), Box<dyn Error>> {
    download_file(
        &lease.image.download_url,
        server,
        device_token,
        client,
        destination,
        control_state,
        TransferReporter::new(local, TransferPhase::Download, lease.image.size_bytes),
    )
}

#[cfg(target_os = "windows")]
fn download_file(
    download_url: &str,
    server: &str,
    device_token: &str,
    client: &Client,
    destination: &Path,
    control_state: &mut impl FnMut() -> Result<JobState, Box<dyn Error>>,
    mut reporter: TransferReporter<'_>,
) -> Result<(), Box<dyn Error>> {
    let parent = destination
        .parent()
        .ok_or("deployment image destination has no parent")?;
    fs::create_dir_all(parent)?;
    let url = if download_url.starts_with("http://") || download_url.starts_with("https://") {
        download_url.to_owned()
    } else {
        format!("{}{}", server.trim_end_matches('/'), download_url)
    };
    let mut response = client
        .get(url)
        .bearer_auth(device_token)
        .send()?
        .error_for_status()?;
    let mut output = File::create(destination)?;
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut downloaded = 0_u64;
    loop {
        await_running(control_state)?;
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        downloaded = downloaded.saturating_add(read as u64);
        reporter.report(downloaded, false);
    }
    output.flush()?;
    reporter.report(downloaded, true);
    Ok(())
}

#[cfg(target_os = "windows")]
fn verify_image(
    path: &Path,
    expected_sha256: &str,
    control_state: &mut impl FnMut() -> Result<JobState, Box<dyn Error>>,
    local: &mut dyn FnMut(LocalProgress),
) -> Result<(), Box<dyn Error>> {
    let mut file = File::open(path)?;
    let mut reporter = TransferReporter::new(
        local,
        TransferPhase::Verify,
        // Display only: an unreadable size must not fail the job.
        file.metadata().map_or(0, |metadata| metadata.len()),
    );
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut verified = 0_u64;
    loop {
        await_running(control_state)?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        verified = verified.saturating_add(read as u64);
        reporter.report(verified, false);
    }
    reporter.report(verified, true);
    let actual = format!("{:x}", digest.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err("downloaded image checksum does not match the approved image".into());
    }
    Ok(())
}

/// Hashes the logical GHO stream while honouring pause and cancel requests.
#[cfg(target_os = "windows")]
struct DigestSink<'a, C> {
    digest: Sha256,
    consumed: u64,
    unchecked: u64,
    control_state: &'a mut C,
    reporter: TransferReporter<'a>,
}

/// Logical stream bytes processed between pause/cancel checks.
#[cfg(target_os = "windows")]
const CONTROL_CHECK_INTERVAL_BYTES: u64 = 4 * 1024 * 1024;

#[cfg(target_os = "windows")]
impl<C> Write for DigestSink<'_, C>
where
    C: FnMut() -> Result<JobState, Box<dyn Error>>,
{
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Writes range from single tag bytes to 1 MiB of run data, so check
        // pause/cancel by volume processed rather than by write size.
        self.unchecked = self.unchecked.saturating_add(bytes.len() as u64);
        if self.unchecked >= CONTROL_CHECK_INTERVAL_BYTES {
            self.unchecked = 0;
            await_running(self.control_state)
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        self.digest.update(bytes);
        self.consumed = self.consumed.saturating_add(bytes.len() as u64);
        if self.unchecked == 0 {
            self.reporter.report(self.consumed, false);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Restores the NTFS volume onto `W:`. Returns whether the target partition is
/// larger than the restored file system, which then needs extending.
#[cfg(target_os = "windows")]
fn restore_gho_partition(
    image_path: &Path,
    metadata: &easydeploymesh_core::AgentGhoDeployment,
    control_state: &mut impl FnMut() -> Result<JobState, Box<dyn Error>>,
    local: &mut dyn FnMut(LocalProgress),
) -> Result<bool, Box<dyn Error>> {
    use std::fs::OpenOptions;

    if metadata.parser_version != easydeploymesh_gho::PARSER_VERSION {
        return Err("GHO parser version does not match the control service".into());
    }
    if metadata.volume_size_bytes == 0 {
        return Err("GHO lease does not state the restored volume size".into());
    }
    let mut image = File::open(image_path)?;
    let mut volume = OpenOptions::new().read(true).write(true).open(r"\\.\W:")?;
    let volume_lock = LockedVolume::acquire(&volume)?;
    let extent = volume_lock.extent()?;
    if extent.bytes_per_sector != 512 {
        return Err(format!(
            "target disk uses {}-byte logical sectors; GHO restore requires 512-byte sectors",
            extent.bytes_per_sector
        )
        .into());
    }
    if extent.length_bytes < metadata.volume_size_bytes {
        return Err(format!(
            "target Windows partition holds {} bytes but the GHO volume needs {}",
            extent.length_bytes, metadata.volume_size_bytes
        )
        .into());
    }
    let hidden_sectors = u32::try_from(extent.starting_offset_bytes / 512)
        .map_err(|_| "target partition starts beyond the NTFS hidden-sector range")?;
    let mut sink = DigestSink {
        digest: Sha256::new(),
        consumed: 0,
        unchecked: 0,
        control_state,
        reporter: TransferReporter::new(
            local,
            TransferPhase::Restore,
            metadata.expanded_size_bytes,
        ),
    };
    let verified = easydeploymesh_gho::restore_partition(
        &mut image,
        metadata.source_partition,
        &mut volume,
        &mut sink,
        easydeploymesh_gho::RestoreOptions {
            target_size_bytes: extent.length_bytes,
            hidden_sectors: Some(hidden_sectors),
            expanded_limit: metadata.expanded_size_bytes,
        },
    )?;
    volume.sync_all()?;
    sink.reporter.report(sink.consumed, true);
    let actual_sha256 = format!("{:x}", sink.digest.finalize());
    if verified.expanded_size_bytes != metadata.expanded_size_bytes
        || sink.consumed != metadata.expanded_size_bytes
    {
        return Err(format!(
            "GHO expanded size mismatch: expected {}, decoded {}",
            metadata.expanded_size_bytes, verified.expanded_size_bytes
        )
        .into());
    }
    if !actual_sha256.eq_ignore_ascii_case(&metadata.expanded_sha256) {
        return Err("GHO expanded SHA-256 mismatch".into());
    }
    if verified.volume_size_bytes() != metadata.volume_size_bytes {
        return Err("GHO restored volume size differs from the approved image".into());
    }
    drop(volume_lock);
    Ok(extent.length_bytes > verified.volume_size_bytes() + verified.geometry.cluster_size)
}

#[cfg(target_os = "windows")]
struct VolumeExtent {
    length_bytes: u64,
    starting_offset_bytes: u64,
    bytes_per_sector: u32,
}

#[cfg(target_os = "windows")]
struct LockedVolume {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(target_os = "windows")]
impl LockedVolume {
    fn acquire(file: &File) -> Result<Self, Box<dyn Error>> {
        use windows::Win32::{
            Foundation::HANDLE,
            System::{
                IO::DeviceIoControl,
                Ioctl::{FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME},
            },
        };

        let handle = HANDLE(file.as_raw_handle());
        unsafe {
            DeviceIoControl(handle, FSCTL_LOCK_VOLUME, None, 0, None, 0, None, None)?;
            if let Err(error) =
                DeviceIoControl(handle, FSCTL_DISMOUNT_VOLUME, None, 0, None, 0, None, None)
            {
                let _ = DeviceIoControl(
                    handle,
                    windows::Win32::System::Ioctl::FSCTL_UNLOCK_VOLUME,
                    None,
                    0,
                    None,
                    0,
                    None,
                    None,
                );
                return Err(error.into());
            }
            // The backup boot sector may sit past the dismounted file
            // system's last sector; lift the volume boundary check.
            if let Err(error) = DeviceIoControl(
                handle,
                windows::Win32::System::Ioctl::FSCTL_ALLOW_EXTENDED_DASD_IO,
                None,
                0,
                None,
                0,
                None,
                None,
            ) {
                let _ = DeviceIoControl(
                    handle,
                    windows::Win32::System::Ioctl::FSCTL_UNLOCK_VOLUME,
                    None,
                    0,
                    None,
                    0,
                    None,
                    None,
                );
                return Err(error.into());
            }
        }
        Ok(Self { handle })
    }

    fn extent(&self) -> Result<VolumeExtent, Box<dyn Error>> {
        use windows::Win32::System::{
            IO::DeviceIoControl,
            Ioctl::{
                DISK_GEOMETRY, GET_LENGTH_INFORMATION, IOCTL_DISK_GET_DRIVE_GEOMETRY,
                IOCTL_DISK_GET_LENGTH_INFO, IOCTL_DISK_GET_PARTITION_INFO_EX,
                PARTITION_INFORMATION_EX,
            },
        };

        let mut length = GET_LENGTH_INFORMATION::default();
        let mut partition = PARTITION_INFORMATION_EX::default();
        let mut geometry = DISK_GEOMETRY::default();
        let mut returned = 0_u32;
        unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_DISK_GET_DRIVE_GEOMETRY,
                None,
                0,
                Some((&mut geometry as *mut DISK_GEOMETRY).cast()),
                size_of::<DISK_GEOMETRY>() as u32,
                Some(&mut returned),
                None,
            )?;
            DeviceIoControl(
                self.handle,
                IOCTL_DISK_GET_LENGTH_INFO,
                None,
                0,
                Some((&mut length as *mut GET_LENGTH_INFORMATION).cast()),
                size_of::<GET_LENGTH_INFORMATION>() as u32,
                Some(&mut returned),
                None,
            )?;
            DeviceIoControl(
                self.handle,
                IOCTL_DISK_GET_PARTITION_INFO_EX,
                None,
                0,
                Some((&mut partition as *mut PARTITION_INFORMATION_EX).cast()),
                size_of::<PARTITION_INFORMATION_EX>() as u32,
                Some(&mut returned),
                None,
            )?;
        }
        Ok(VolumeExtent {
            length_bytes: u64::try_from(length.Length).map_err(|_| "volume length is negative")?,
            starting_offset_bytes: u64::try_from(partition.StartingOffset)
                .map_err(|_| "partition offset is negative")?,
            bytes_per_sector: geometry.BytesPerSector,
        })
    }
}

#[cfg(target_os = "windows")]
impl Drop for LockedVolume {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::IO::DeviceIoControl(
                self.handle,
                windows::Win32::System::Ioctl::FSCTL_UNLOCK_VOLUME,
                None,
                0,
                None,
                0,
                None,
                None,
            );
        }
    }
}

#[cfg(target_os = "windows")]
fn await_running(
    control_state: &mut impl FnMut() -> Result<JobState, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    loop {
        match control_state()? {
            JobState::Running => return Ok(()),
            JobState::Paused => thread::sleep(Duration::from_secs(1)),
            state => {
                return Err(
                    format!("deployment control entered unexpected state: {state:?}").into(),
                );
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn set_process_suspended(
    process_handle: *mut c_void,
    suspended: bool,
) -> Result<(), Box<dyn Error>> {
    let status = unsafe {
        if suspended {
            NtSuspendProcess(process_handle)
        } else {
            NtResumeProcess(process_handle)
        }
    };
    if status < 0 {
        return Err(format!(
            "Windows process {} failed with NTSTATUS 0x{status:08x}",
            if suspended { "suspension" } else { "resume" }
        )
        .into());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn run_checked<P, I, S>(
    program: P,
    arguments: I,
    control_state: &mut impl FnMut() -> Result<JobState, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program_name = program.as_ref().to_string_lossy().into_owned();
    let mut child = Command::new(program.as_ref())
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().map(spawn_output_reader);
    let stderr = child.stderr.take().map(spawn_output_reader);
    let process_handle = child.as_raw_handle() as *mut c_void;
    let mut suspended = false;
    loop {
        let requested_state = match control_state() {
            Ok(state) => state,
            Err(_) => {
                // Keep the last applied process state during a transient control-plane outage.
                // Resuming a deliberately suspended disk writer without operator intent is unsafe.
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        match requested_state {
            JobState::Paused if !suspended => {
                if let Err(error) = set_process_suspended(process_handle, true) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
                suspended = true;
            }
            JobState::Running if suspended => {
                if let Err(error) = set_process_suspended(process_handle, false) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
                suspended = false;
            }
            JobState::Running | JobState::Paused => {}
            state => {
                if suspended {
                    let _ = set_process_suspended(process_handle, false);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    format!("deployment control entered unexpected state: {state:?}").into(),
                );
            }
        }
        if let Some(status) = child.try_wait()? {
            let output = process_output(stdout, stderr);
            return if status.success() {
                Ok(())
            } else {
                Err(format!("{program_name} failed ({status}){output}").into())
            };
        }
        thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(target_os = "windows")]
fn spawn_output_reader(mut reader: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        const OUTPUT_LIMIT: usize = 8 * 1024;
        let mut captured = Vec::with_capacity(OUTPUT_LIMIT);
        let mut buffer = [0_u8; 1024];
        loop {
            let Ok(count) = reader.read(&mut buffer) else {
                break;
            };
            if count == 0 {
                break;
            }
            let remaining = OUTPUT_LIMIT.saturating_sub(captured.len());
            captured.extend_from_slice(&buffer[..count.min(remaining)]);
        }
        captured
    })
}

#[cfg(target_os = "windows")]
fn process_output(
    stdout: Option<thread::JoinHandle<Vec<u8>>>,
    stderr: Option<thread::JoinHandle<Vec<u8>>>,
) -> String {
    let mut bytes = stdout
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    if let Some(mut error) = stderr.and_then(|reader| reader.join().ok()) {
        if !bytes.is_empty() && !error.is_empty() {
            bytes.push(b'\n');
        }
        bytes.append(&mut error);
    }
    let output = String::from_utf8_lossy(&bytes)
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .collect::<String>();
    let output = output.trim();
    if output.is_empty() {
        String::new()
    } else {
        format!("; output: {output}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use easydeploymesh_core::{
        AgentDeploymentImage, AgentGhoDeployment, DeploymentTarget, PartitionPlan,
    };
    use uuid::Uuid;

    #[test]
    fn image_boot_policy_is_reapplied_to_the_new_store_only_when_present() {
        let disabled = LoaderIntegrityPolicy {
            disable_integrity_checks: true,
        };
        assert!(boot_policy_commands("BIOS", LoaderIntegrityPolicy::default()).is_empty());
        assert_eq!(
            boot_policy_commands("BIOS", disabled),
            vec![
                vec![
                    "/store",
                    r"S:\Boot\BCD",
                    "/set",
                    "{default}",
                    "loadoptions",
                    "DISABLE_INTEGRITY_CHECKS"
                ],
                vec![
                    "/store",
                    r"S:\Boot\BCD",
                    "/set",
                    "{default}",
                    "nointegritychecks",
                    "Yes"
                ],
            ]
        );
        assert!(
            boot_policy_commands("UEFI", disabled)
                .iter()
                .all(|arguments| arguments[1] == r"S:\EFI\Microsoft\Boot\BCD")
        );
    }

    #[test]
    fn a_missing_or_unreadable_image_boot_store_carries_no_policy() {
        let directory = std::env::temp_dir().join(format!("edm-bcd-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let missing = directory.join("missing");
        assert_eq!(
            read_image_boot_policy(&missing),
            LoaderIntegrityPolicy::default()
        );
        let garbage = directory.join("BCD");
        std::fs::write(&garbage, b"not a registry hive").unwrap();
        assert_eq!(
            read_image_boot_policy(&garbage),
            LoaderIntegrityPolicy::default()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn lease(plan: PartitionPlan) -> AgentJobLease {
        AgentJobLease {
            job_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            expires_at: Utc::now() + Duration::hours(1),
            operation: Operation::DeployWim,
            image: AgentDeploymentImage {
                id: Uuid::new_v4(),
                name: "Windows 10".to_owned(),
                format: ImageFormat::Wim,
                size_bytes: 5 * 1024 * 1024 * 1024,
                sha256: "00".repeat(32),
                download_url: "/image".to_owned(),
                index: 1,
            },
            target: DeploymentTarget {
                device_id: Uuid::new_v4(),
                target_disk_id: r"\\.\PhysicalDrive0".to_owned(),
                target_disk_model: "VMware Virtual Disk".to_owned(),
                target_disk_serial: Some("VM-DISK-0".to_owned()),
                target_disk_size_bytes: 60 * 1024 * 1024 * 1024,
            },
            partition_plan: plan,
            gho: None,
        }
    }

    fn disk() -> Disk {
        Disk {
            id: r"\\.\PHYSICALDRIVE0".to_owned(),
            model: "VMware Virtual Disk".to_owned(),
            serial: Some("VM-DISK-0".to_owned()),
            size_bytes: 60 * 1024 * 1024 * 1024,
            is_system: false,
        }
    }

    #[test]
    fn legacy_layout_is_generated_without_running_diskpart() {
        let prepared = prepare_layout(&lease(PartitionPlan::legacy_bios_mbr()), &disk())
            .expect("layout should be generated");
        assert!(
            prepared
                .prepare_script
                .contains("select disk 0\nclean\nconvert mbr")
        );
        assert!(prepared.prepare_script.contains("active"));
        assert!(prepared.prepare_script.contains("assign letter=W"));
        assert!(prepared.prepare_script.contains("EasyDeployMesh Cache"));
        assert_eq!(prepared.firmware, "BIOS");
    }

    #[test]
    fn oversized_gho_partition_fails_before_a_diskpart_script_is_available() {
        let mut deployment = lease(PartitionPlan::uefi_gpt());
        deployment.operation = Operation::DeployGho;
        deployment.image.format = ImageFormat::Gho;
        deployment.gho = Some(AgentGhoDeployment {
            source_partition: 1,
            expanded_size_bytes: 4 * 1024 * 1024 * 1024,
            expanded_sha256: "11".repeat(32),
            volume_size_bytes: 56 * 1024 * 1024 * 1024,
            parser_version: easydeploymesh_gho::PARSER_VERSION,
        });

        let error = prepare_layout(&deployment, &disk())
            .expect_err("raw GHO payload must fit before partitioning")
            .to_string();
        assert!(error.contains("raw Windows partition payload"));
    }

    #[test]
    fn gho_lease_without_a_volume_size_is_rejected() {
        let mut deployment = lease(PartitionPlan::uefi_gpt());
        deployment.operation = Operation::DeployGho;
        deployment.image.format = ImageFormat::Gho;
        deployment.gho = Some(AgentGhoDeployment {
            source_partition: 1,
            expanded_size_bytes: 1024,
            expanded_sha256: "11".repeat(32),
            volume_size_bytes: 0,
            parser_version: easydeploymesh_gho::PARSER_VERSION,
        });

        let error = prepare_layout(&deployment, &disk())
            .expect_err("a GHO lease must carry the volume size")
            .to_string();
        assert!(error.contains("restored volume size"));
    }

    #[test]
    fn custom_windows_and_data_layout_keeps_data_as_the_remaining_partition() {
        let mut plan = PartitionPlan::uefi_gpt();
        plan.partitions.last_mut().unwrap().size_mib = Some(30 * 1024);
        plan.partitions.push(easydeploymesh_core::PartitionSpec {
            role: PartitionRole::Data,
            size_mib: None,
            file_system: Some(PartitionFileSystem::Ntfs),
            label: "Data".to_owned(),
            drive_letter: Some('D'),
        });

        let mut target_disk = disk();
        target_disk.size_bytes = 100 * 1024 * 1024 * 1024;
        let prepared =
            prepare_layout(&lease(plan), &target_disk).expect("layout should be generated");
        assert!(prepared.prepare_script.contains("size=30720"));
        assert!(prepared.prepare_script.contains("assign letter=D"));
        assert!(prepared.prepare_script.contains("assign letter=R"));
        assert!(prepared.cleanup_script.contains("select volume R"));
        assert!(prepared.cleanup_script.contains("select volume D\nextend"));
        assert!(prepared.image_path.to_string_lossy().starts_with(r"R:\"));
    }

    #[test]
    fn invalid_drive_letters_fail_before_a_diskpart_script_is_available() {
        let data = |size_mib, drive_letter| easydeploymesh_core::PartitionSpec {
            role: PartitionRole::Data,
            size_mib,
            file_system: Some(PartitionFileSystem::Ntfs),
            label: "Data".to_owned(),
            drive_letter,
        };
        let mut target_disk = disk();
        target_disk.size_bytes = 100 * 1024 * 1024 * 1024;
        for data_partitions in [
            vec![data(None, Some('S'))],
            vec![data(None, Some('x'))],
            vec![data(Some(10 * 1024), None), data(None, None)],
            vec![data(Some(10 * 1024), None), data(None, Some('D'))],
        ] {
            let mut plan = PartitionPlan::uefi_gpt();
            plan.partitions.last_mut().unwrap().size_mib = Some(30 * 1024);
            plan.partitions.extend(data_partitions);
            let error = prepare_layout(&lease(plan), &target_disk)
                .expect_err("an invalid plan must not produce a DiskPart script")
                .to_string();
            assert!(error.contains("drive letter"), "{error}");
        }
    }

    #[test]
    fn implicit_data_drive_letter_matches_the_shared_default() {
        let mut plan = PartitionPlan::uefi_gpt();
        plan.partitions.last_mut().unwrap().size_mib = Some(30 * 1024);
        plan.partitions.push(easydeploymesh_core::PartitionSpec {
            role: PartitionRole::Data,
            size_mib: None,
            file_system: Some(PartitionFileSystem::Ntfs),
            label: "Data".to_owned(),
            drive_letter: None,
        });
        let mut target_disk = disk();
        target_disk.size_bytes = 100 * 1024 * 1024 * 1024;
        let prepared =
            prepare_layout(&lease(plan), &target_disk).expect("layout should be generated");
        let default = easydeploymesh_core::DEFAULT_DATA_DRIVE_LETTER;
        assert!(
            prepared
                .prepare_script
                .contains(&format!("assign letter={default}\n"))
        );
        assert!(
            prepared
                .cleanup_script
                .contains(&format!("select volume {default}\nextend"))
        );
    }

    #[test]
    fn legacy_three_volume_layout_does_not_exceed_the_mbr_primary_partition_limit() {
        let mut plan = PartitionPlan::legacy_bios_mbr();
        plan.partitions.last_mut().unwrap().size_mib = Some(30 * 1024);
        plan.partitions.extend([
            easydeploymesh_core::PartitionSpec {
                role: PartitionRole::Data,
                size_mib: Some(40 * 1024),
                file_system: Some(PartitionFileSystem::Ntfs),
                label: "Software".to_owned(),
                drive_letter: Some('D'),
            },
            easydeploymesh_core::PartitionSpec {
                role: PartitionRole::Data,
                size_mib: None,
                file_system: Some(PartitionFileSystem::Ntfs),
                label: "Data".to_owned(),
                drive_letter: Some('E'),
            },
        ]);

        let mut target_disk = disk();
        target_disk.size_bytes = 100 * 1024 * 1024 * 1024;
        let prepared =
            prepare_layout(&lease(plan), &target_disk).expect("layout should be generated");
        let primary_count = prepared
            .prepare_script
            .lines()
            .filter(|line| {
                *line == "create partition primary"
                    || line.starts_with("create partition primary size=")
            })
            .count();

        assert!(
            primary_count <= 4,
            "MBR script exceeded four primary partitions:\n{}",
            prepared.prepare_script
        );
        assert!(
            prepared
                .prepare_script
                .contains("create partition extended")
        );
        assert!(prepared.prepare_script.contains("create partition logical"));
        assert_eq!(
            prepared
                .prepare_script
                .lines()
                .filter(|line| line.starts_with("create partition logical"))
                .count(),
            3,
            "D, E, and the temporary cache must be logical volumes"
        );
        assert!(prepared.cleanup_script.contains("select volume E\nextend"));
    }

    #[test]
    fn uefi_three_volume_layout_remains_a_gpt_layout() {
        let mut plan = PartitionPlan::uefi_gpt();
        plan.partitions.last_mut().unwrap().size_mib = Some(30 * 1024);
        plan.partitions.extend([
            easydeploymesh_core::PartitionSpec {
                role: PartitionRole::Data,
                size_mib: Some(40 * 1024),
                file_system: Some(PartitionFileSystem::Ntfs),
                label: "Software".to_owned(),
                drive_letter: Some('D'),
            },
            easydeploymesh_core::PartitionSpec {
                role: PartitionRole::Data,
                size_mib: None,
                file_system: Some(PartitionFileSystem::Ntfs),
                label: "Data".to_owned(),
                drive_letter: Some('E'),
            },
        ]);
        let mut target_disk = disk();
        target_disk.size_bytes = 100_000_000_000;

        let prepared =
            prepare_layout(&lease(plan), &target_disk).expect("GPT layout should be generated");

        assert!(prepared.prepare_script.contains("convert gpt"));
        assert!(!prepared.prepare_script.contains("partition extended"));
        assert!(!prepared.prepare_script.contains("partition logical"));
        assert!(prepared.cleanup_script.contains("select volume E\nextend"));
    }

    #[test]
    fn insufficient_capacity_fails_before_a_diskpart_script_is_available() {
        let mut plan = PartitionPlan::legacy_bios_mbr();
        plan.partitions.last_mut().unwrap().size_mib = Some(30 * 1024);
        plan.partitions.extend([
            easydeploymesh_core::PartitionSpec {
                role: PartitionRole::Data,
                size_mib: Some(70 * 1024),
                file_system: Some(PartitionFileSystem::Ntfs),
                label: "Software".to_owned(),
                drive_letter: Some('D'),
            },
            easydeploymesh_core::PartitionSpec {
                role: PartitionRole::Data,
                size_mib: None,
                file_system: Some(PartitionFileSystem::Ntfs),
                label: "Data".to_owned(),
                drive_letter: Some('E'),
            },
        ]);
        let mut target_disk = disk();
        target_disk.size_bytes = 100_000_000_000;

        let error = prepare_layout(&lease(plan), &target_disk)
            .expect_err("capacity validation must stop layout generation");

        assert!(error.to_string().contains("partition plan requires"));
    }

    #[test]
    fn disk_fingerprint_change_stops_before_partitioning() {
        let mut changed = disk();
        changed.serial = Some("OTHER".to_owned());
        let lease = lease(PartitionPlan::legacy_bios_mbr());
        assert!(matching_disk(&lease, &[changed]).is_err());
    }

    #[test]
    fn system_disk_is_rejected_before_partitioning() {
        let mut system = disk();
        system.is_system = true;
        let lease = lease(PartitionPlan::legacy_bios_mbr());

        let error = matching_disk(&lease, &[system])
            .expect_err("the running system disk must never be selected for deployment");

        assert!(error.to_string().contains("running system"));
        assert!(matching_disk(&lease, &[disk()]).is_ok());
    }
}
