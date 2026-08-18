use easydeploymesh_core::{
    AgentJobLease, DeploymentStage, Disk, ImageFormat, JobState, Operation, PartitionFileSystem,
    PartitionRole, PartitionTable,
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
    process::Command,
    thread,
    time::Duration,
};

#[cfg(target_os = "windows")]
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSuspendProcess(process_handle: *mut c_void) -> i32;
    fn NtResumeProcess(process_handle: *mut c_void) -> i32;
}

const MIB: u64 = 1024 * 1024;
const MINIMUM_WINDOWS_MIB: u64 = 20 * 1024;
const IMAGE_CACHE_HEADROOM_MIB: u64 = 512;
const PARTITION_ALIGNMENT_HEADROOM_MIB: u64 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedLayout {
    prepare_script: String,
    cleanup_script: String,
    image_path: PathBuf,
    firmware: &'static str,
    disk_number: u32,
    windows_partition: u32,
}

pub fn execute(
    lease: &AgentJobLease,
    inventory_disks: &[Disk],
    server: &str,
    device_token: &str,
    client: &Client,
    mut progress: impl FnMut(DeploymentStage, u8, &str) -> Result<(), Box<dyn Error>>,
    #[allow(unused_mut)] mut control_state: impl FnMut() -> Result<JobState, Box<dyn Error>>,
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

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (server, device_token, client, layout, control_state);
        Err("deployment execution is available only inside Windows PE".into())
    }

    #[cfg(target_os = "windows")]
    {
        let work_directory =
            PathBuf::from(r"X:\EasyDeployMesh\work").join(lease.job_id.to_string());
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
        )?;
        verify_image(&layout.image_path, &lease.image.sha256, &mut control_state)?;

        progress(
            DeploymentStage::ApplyingImage,
            45,
            "Applying the Windows image",
        )?;
        if lease.operation == Operation::DeployGho {
            let gho = lease
                .gho
                .as_ref()
                .ok_or("GHO deployment metadata is missing")?;
            await_running(&mut control_state)?;
            restore_gho_partition(&layout.image_path, gho)?;
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
                    OsString::from(r"/ApplyDir:W:\"),
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
                OsStr::new(r"W:\Windows"),
                OsStr::new("/s"),
                OsStr::new("S:"),
                OsStr::new("/f"),
                OsStr::new(layout.firmware),
                OsStr::new("/c"),
            ],
            &mut control_state,
        )?;

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

fn matching_disk<'a>(lease: &AgentJobLease, disks: &'a [Disk]) -> Result<&'a Disk, Box<dyn Error>> {
    let target = &lease.target;
    let disk = disks
        .iter()
        .find(|disk| disk.id.eq_ignore_ascii_case(&target.target_disk_id))
        .ok_or("the selected physical disk is no longer present")?;
    if !target.matches_disk(disk) {
        return Err("target disk fingerprint changed after the deployment was confirmed".into());
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
        .div_ceil(MIB)
        .saturating_add(IMAGE_CACHE_HEADROOM_MIB);
    let disk_mib = disk.size_bytes / MIB;
    let remaining_role = lease
        .partition_plan
        .partitions
        .iter()
        .find(|partition| partition.size_mib.is_none())
        .map(|partition| partition.role)
        .ok_or("partition plan has no remaining-space partition")?;
    let remaining_minimum_mib = if remaining_role == PartitionRole::Windows {
        MINIMUM_WINDOWS_MIB
    } else {
        1024
    };
    let required_mib = fixed_mib
        .saturating_add(cache_mib)
        .saturating_add(remaining_minimum_mib)
        .saturating_add(PARTITION_ALIGNMENT_HEADROOM_MIB);
    if disk_mib < required_mib {
        return Err(
            format!("target disk is too small: requires at least {required_mib} MiB").into(),
        );
    }
    let remaining_mib = disk_mib - fixed_mib - cache_mib - PARTITION_ALIGNMENT_HEADROOM_MIB;
    let mut script = format!(
        "select disk {disk_number}\nclean\nconvert {}\n",
        match lease.partition_plan.table {
            PartitionTable::Gpt => "gpt",
            PartitionTable::Mbr => "mbr",
        }
    );
    let mut windows_partition = 0_u32;
    for (index, partition) in lease.partition_plan.partitions.iter().enumerate() {
        if partition.role == PartitionRole::Windows {
            windows_partition = u32::try_from(index + 1)?;
        }
        if partition.size_mib.is_none() {
            script.push_str(&format!("create partition primary size={remaining_mib}\n"));
        } else {
            let size = partition.size_mib.ok_or("partition size is missing")?;
            let kind = match partition.role {
                PartitionRole::Efi => "efi",
                PartitionRole::Msr => "msr",
                _ => "primary",
            };
            script.push_str(&format!("create partition {kind} size={size}\n"));
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
            PartitionRole::Efi => script.push_str("assign letter=S\n"),
            PartitionRole::System => {
                if lease.partition_plan.table == PartitionTable::Mbr {
                    script.push_str("active\n");
                }
                script.push_str("assign letter=S\n");
            }
            PartitionRole::Windows => script.push_str("assign letter=W\n"),
            PartitionRole::Data => {
                let letter = partition.drive_letter.unwrap_or('D').to_ascii_uppercase();
                script.push_str(&format!("assign letter={letter}\n"));
            }
            _ => {}
        }
    }
    script.push_str(&format!(
        "create partition primary size={cache_mib}\nformat quick fs=ntfs label=\"EasyDeployMesh Cache\"\nassign letter=R\nexit\n"
    ));
    let extend_letter = if remaining_role == PartitionRole::Data {
        lease
            .partition_plan
            .partitions
            .iter()
            .find(|partition| partition.size_mib.is_none())
            .and_then(|partition| partition.drive_letter)
            .unwrap_or('D')
    } else {
        'W'
    };
    let cleanup_script = format!(
        "select disk {disk_number}\nselect volume R\ndelete volume\nselect volume {extend_letter}\nextend\nexit\n"
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
        image_path: PathBuf::from(format!(r"R:\EasyDeployMesh\image.{extension}")),
        firmware: match lease.partition_plan.table {
            PartitionTable::Gpt => "UEFI",
            PartitionTable::Mbr => "BIOS",
        },
        disk_number,
        windows_partition,
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

#[cfg(target_os = "windows")]
fn download_image(
    lease: &AgentJobLease,
    server: &str,
    device_token: &str,
    client: &Client,
    destination: &Path,
    control_state: &mut impl FnMut() -> Result<JobState, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    download_file(
        &lease.image.download_url,
        server,
        device_token,
        client,
        destination,
        control_state,
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
    loop {
        await_running(control_state)?;
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
    }
    output.flush()?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn verify_image(
    path: &Path,
    expected_sha256: &str,
    control_state: &mut impl FnMut() -> Result<JobState, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        await_running(control_state)?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err("downloaded image checksum does not match the approved image".into());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
struct DigestWriter<W> {
    inner: W,
    digest: Sha256,
    written: u64,
}

#[cfg(target_os = "windows")]
impl<W: Write> Write for DigestWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.digest.update(&bytes[..count]);
        self.written = self.written.saturating_add(count as u64);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(target_os = "windows")]
fn restore_gho_partition(
    image_path: &Path,
    metadata: &easydeploymesh_core::AgentGhoDeployment,
) -> Result<(), Box<dyn Error>> {
    use std::fs::OpenOptions;

    if metadata.parser_version != easydeploymesh_gho::PARSER_VERSION {
        return Err("GHO parser version does not match the control service".into());
    }
    let mut image = File::open(image_path)?;
    let volume = OpenOptions::new().read(true).write(true).open(r"\\.\W:")?;
    let volume_lock = LockedVolume::acquire(&volume)?;
    let mut output = DigestWriter {
        inner: volume,
        digest: Sha256::new(),
        written: 0,
    };
    let (_, decoded) = easydeploymesh_gho::decode_partition(
        &mut image,
        metadata.source_partition,
        &mut output,
        metadata.expanded_size_bytes,
    )?;
    output.flush()?;
    output.inner.sync_all()?;
    let actual_sha256 = format!("{:x}", output.digest.finalize());
    if decoded != metadata.expanded_size_bytes || output.written != metadata.expanded_size_bytes {
        return Err(format!(
            "GHO expanded size mismatch: expected {}, decoded {decoded}, written {}",
            metadata.expanded_size_bytes, output.written
        )
        .into());
    }
    if !actual_sha256.eq_ignore_ascii_case(&metadata.expanded_sha256) {
        return Err("GHO expanded SHA-256 mismatch".into());
    }
    drop(volume_lock);
    Ok(())
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
        }
        Ok(Self { handle })
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
    let mut child = Command::new(program.as_ref()).args(arguments).spawn()?;
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
            return if status.success() {
                Ok(())
            } else {
                Err(format!("{program_name} failed ({status})").into())
            };
        }
        thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use easydeploymesh_core::{AgentDeploymentImage, DeploymentTarget, PartitionPlan};
    use uuid::Uuid;

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

        let prepared = prepare_layout(&lease(plan), &disk()).expect("layout should be generated");
        assert!(prepared.prepare_script.contains("size=30720"));
        assert!(prepared.prepare_script.contains("assign letter=D"));
        assert!(prepared.prepare_script.contains("assign letter=R"));
        assert!(prepared.cleanup_script.contains("select volume R"));
        assert!(prepared.cleanup_script.contains("select volume D\nextend"));
        assert!(prepared.image_path.to_string_lossy().starts_with(r"R:\"));
    }

    #[test]
    fn disk_fingerprint_change_stops_before_partitioning() {
        let mut changed = disk();
        changed.serial = Some("OTHER".to_owned());
        let lease = lease(PartitionPlan::legacy_bios_mbr());
        assert!(matching_disk(&lease, &[changed]).is_err());
    }
}
