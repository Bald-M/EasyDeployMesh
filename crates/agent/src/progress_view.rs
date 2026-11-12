//! Display model for the WinPE deployment progress window.
//!
//! Everything here is presentation only: it turns the stages the executor reports
//! to the control service, plus local byte counters, into localized text and a
//! fixed pixel layout. It never influences the deployment flow. The Win32 drawing
//! lives in `winpe_progress`; this module stays platform independent so the rules
//! (especially the disk-state rule after a failure) are unit tested everywhere.

use crate::deployment::{LocalProgress, TargetLayout, TransferPhase};
use easydeploymesh_core::{AgentJobLease, DeploymentStage, Operation, PartitionFileSystem};
use std::time::{Duration, Instant};

pub const TOTAL_STEPS: u8 = 5;
const REASON_CHARACTER_LIMIT: usize = 160;
const MINIMUM_RATE_WINDOW: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Chinese,
    English,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    Reboot,
    Shutdown,
}

/// Facts about the claimed job that are known before execution starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobInfo {
    pub device_name: String,
    pub image_name: String,
    /// GHO jobs restore a partition image; WIM/ESD jobs deploy one.
    pub is_restore: bool,
    pub image_bytes: u64,
    /// Bytes written to the Windows partition when the executor knows them up
    /// front (the expanded size of a GHO partition).
    pub expanded_bytes: Option<u64>,
    pub control_service: String,
    pub log_path: String,
}

impl JobInfo {
    pub fn for_lease(
        lease: &AgentJobLease,
        server: &str,
        device_name: String,
        log_path: String,
    ) -> Self {
        let control_service = server
            .trim()
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_owned();
        Self {
            device_name,
            image_name: lease.image.name.clone(),
            is_restore: lease.operation == Operation::DeployGho,
            image_bytes: lease.image.size_bytes,
            expanded_bytes: lease.gho.as_ref().map(|gho| gho.expanded_size_bytes),
            control_service,
            log_path,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskState {
    /// Partitioning had certainly not started when the job failed.
    Untouched,
    /// Partitioning started, or it is not known that it had not.
    PartiallyWritten,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    DiskMissing,
    DiskChanged,
    Checksum,
    Cancelled,
    Network,
    DiskPart,
    Dism,
    BcdBoot,
    Other,
}

#[derive(Clone, Debug)]
struct Transfer {
    phase: TransferPhase,
    done: u64,
    total: u64,
    started: Instant,
    updated: Instant,
}

#[derive(Clone, Debug)]
struct Failure {
    step: u8,
    step_label: &'static StepLabel,
    percent: u8,
    kind: FailureKind,
    message: String,
    disk_state: DiskState,
}

#[derive(Clone, Debug)]
enum Outcome {
    Running,
    Succeeded {
        finished: Instant,
        reboot_at: Instant,
    },
    Failed {
        finished: Instant,
        failure: Failure,
        /// Whether the control service confirmed the failure report.
        reported: bool,
    },
}

#[derive(Clone, Debug)]
pub struct ProgressModel {
    language: Language,
    job: JobInfo,
    layout: Option<TargetLayout>,
    stage: DeploymentStage,
    stage_percent: u8,
    transfer: Option<Transfer>,
    disk_may_be_modified: bool,
    started: Instant,
    outcome: Outcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Running,
    Success,
    Failure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueStyle {
    Normal,
    Monospace,
    Positive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub value: String,
    pub style: ValueStyle,
    /// Text lines the value may wrap onto (1 or 2).
    pub lines: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerTone {
    Warning,
    Danger,
    Neutral,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    pub text: String,
    pub tone: BannerTone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    pub action: PowerAction,
    pub label: String,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Footer {
    pub text: String,
    pub buttons: Vec<Button>,
}

/// Fully localized content of one frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub tone: Tone,
    pub product: &'static str,
    pub device: String,
    pub title: String,
    pub step_caption: Option<String>,
    pub percent: u8,
    pub percent_label: Option<String>,
    pub remaining_label: Option<String>,
    pub summary: Option<String>,
    pub columns: Vec<Vec<Row>>,
    pub banner: Option<Banner>,
    pub footer: Option<Footer>,
}

#[derive(Debug)]
struct StepLabel {
    chinese: &'static str,
    english: &'static str,
}

const STEP_CHECK: StepLabel = StepLabel {
    chinese: "检查目标硬盘",
    english: "Checking the target disk",
};
const STEP_PARTITION: StepLabel = StepLabel {
    chinese: "分区",
    english: "Partitioning the disk",
};
const STEP_DOWNLOAD: StepLabel = StepLabel {
    chinese: "下载镜像",
    english: "Downloading the image",
};
const STEP_VERIFY: StepLabel = StepLabel {
    chinese: "校验镜像",
    english: "Verifying the image",
};
const STEP_WRITE: StepLabel = StepLabel {
    chinese: "写入系统",
    english: "Writing the system",
};
const STEP_BOOT: StepLabel = StepLabel {
    chinese: "配置引导",
    english: "Configuring boot",
};

impl ProgressModel {
    /// `disk_touched_earlier` carries over whether an earlier job in this Agent
    /// run may have written to a disk, so a later failure never claims the disk
    /// is untouched.
    pub fn new(language: Language, job: JobInfo, disk_touched_earlier: bool, now: Instant) -> Self {
        Self {
            language,
            job,
            layout: None,
            stage: DeploymentStage::Preflight,
            stage_percent: 0,
            transfer: None,
            disk_may_be_modified: disk_touched_earlier,
            started: now,
            outcome: Outcome::Running,
        }
    }

    /// Records a stage update. The executor reports a stage before doing its work,
    /// so seeing the partitioning stage (or any later one) means the disk may have
    /// been modified.
    pub fn stage(&mut self, stage: DeploymentStage, percent: u8) {
        if !matches!(self.outcome, Outcome::Running) {
            return;
        }
        if step_number(stage) >= step_number(DeploymentStage::Partitioning) {
            self.disk_may_be_modified = true;
        }
        if stage != self.stage {
            self.transfer = None;
        }
        self.stage = stage;
        self.stage_percent = percent.min(100);
    }

    pub fn local(&mut self, event: &LocalProgress, now: Instant) {
        if !matches!(self.outcome, Outcome::Running) {
            return;
        }
        match event {
            LocalProgress::Layout(layout) => self.layout = Some(layout.clone()),
            LocalProgress::DiskWriteStarting => self.disk_may_be_modified = true,
            LocalProgress::Transfer { phase, done, total } => {
                let started = match &self.transfer {
                    Some(transfer) if transfer.phase == *phase => transfer.started,
                    _ => now,
                };
                self.transfer = Some(Transfer {
                    phase: *phase,
                    done: *done,
                    total: *total,
                    started,
                    updated: now,
                });
            }
        }
    }

    pub fn succeed(&mut self, now: Instant, reboot_delay: Duration) {
        self.outcome = Outcome::Succeeded {
            finished: now,
            reboot_at: now + reboot_delay,
        };
    }

    pub fn fail(&mut self, message: &str, now: Instant) {
        if !matches!(self.outcome, Outcome::Running) {
            return;
        }
        let failure = Failure {
            step: step_number(self.stage),
            step_label: self.step_label(),
            percent: self.display_percent(),
            kind: classify_failure(message),
            message: bounded(message.trim(), REASON_CHARACTER_LIMIT),
            disk_state: if self.disk_may_be_modified {
                DiskState::PartiallyWritten
            } else {
                DiskState::Untouched
            },
        };
        self.outcome = Outcome::Failed {
            finished: now,
            failure,
            reported: false,
        };
    }

    /// Records that the control service accepted the failure report.
    pub fn failure_reported(&mut self) {
        if let Outcome::Failed { reported, .. } = &mut self.outcome {
            *reported = true;
        }
    }

    pub fn is_succeeded(&self) -> bool {
        matches!(self.outcome, Outcome::Succeeded { .. })
    }

    pub fn is_failed(&self) -> bool {
        matches!(self.outcome, Outcome::Failed { .. })
    }

    pub fn disk_may_be_modified(&self) -> bool {
        self.disk_may_be_modified
    }

    #[cfg(test)]
    pub fn disk_state_after_failure(&self) -> Option<DiskState> {
        match &self.outcome {
            Outcome::Failed { failure, .. } => Some(failure.disk_state),
            _ => None,
        }
    }

    /// Percent shown locally. It follows the reported stage percent and moves
    /// smoothly inside the long transfer stages; the reported value is unchanged.
    pub fn display_percent(&self) -> u8 {
        let Some(transfer) = &self.transfer else {
            return self.stage_percent;
        };
        let (start, end) = match (self.stage, transfer.phase) {
            (DeploymentStage::DownloadingImage, TransferPhase::Download) => (20, 40),
            (DeploymentStage::DownloadingImage, TransferPhase::Verify) => (40, 45),
            (DeploymentStage::ApplyingImage, TransferPhase::Restore) => (45, 88),
            _ => return self.stage_percent,
        };
        if transfer.total == 0 {
            return self.stage_percent;
        }
        let fraction = transfer.done.min(transfer.total) as f64 / transfer.total as f64;
        let interpolated = start as f64 + (end - start) as f64 * fraction;
        self.stage_percent.max(interpolated.floor() as u8).min(100)
    }

    fn step_label(&self) -> &'static StepLabel {
        if self.stage == DeploymentStage::DownloadingImage
            && self
                .transfer
                .as_ref()
                .is_some_and(|transfer| transfer.phase == TransferPhase::Verify)
        {
            return &STEP_VERIFY;
        }
        match step_number(self.stage) {
            1 => &STEP_CHECK,
            2 => &STEP_PARTITION,
            3 => &STEP_DOWNLOAD,
            4 => &STEP_WRITE,
            _ => &STEP_BOOT,
        }
    }

    fn text(&self, chinese: &'static str, english: &'static str) -> &'static str {
        match self.language {
            Language::Chinese => chinese,
            Language::English => english,
        }
    }

    fn label(&self, label: &StepLabel) -> &'static str {
        self.text(label.chinese, label.english)
    }

    fn row(&self, chinese: &'static str, english: &'static str, value: String) -> Row {
        Row {
            label: self.text(chinese, english).to_owned(),
            value,
            style: ValueStyle::Normal,
            lines: 1,
        }
    }

    fn verb_title(
        &self,
        deploy: (&'static str, &'static str),
        restore: (&'static str, &'static str),
    ) -> String {
        let (chinese, english) = if self.job.is_restore { restore } else { deploy };
        self.text(chinese, english).to_owned()
    }

    fn disk_text(&self) -> String {
        self.layout.as_ref().map_or_else(
            || "—".to_owned(),
            |layout| format!("{} {}", self.text("磁盘", "Disk"), layout.disk_number),
        )
    }

    fn disk_model_text(&self) -> String {
        self.layout
            .as_ref()
            .map(|layout| layout.disk_model.trim())
            .filter(|model| !model.is_empty())
            .map_or_else(|| "—".to_owned(), str::to_owned)
    }

    fn partition_text(&self, with_size: bool) -> String {
        let Some(layout) = &self.layout else {
            return "—".to_owned();
        };
        let mut parts = vec![format!(
            "{} {}",
            self.text("分区", "Partition"),
            layout.windows_partition
        )];
        if let Some(file_system) = layout.file_system {
            parts.push(file_system_name(file_system).to_owned());
        }
        if with_size && layout.windows_partition_bytes > 0 {
            parts.push(format_bytes(layout.windows_partition_bytes));
        }
        parts.join(" · ")
    }

    fn target_text(&self, with_file_system: bool) -> String {
        let Some(layout) = &self.layout else {
            return "—".to_owned();
        };
        let mut parts = vec![
            format!("{} {}", self.text("磁盘", "Disk"), layout.disk_number),
            format!(
                "{} {}",
                self.text("分区", "Partition"),
                layout.windows_partition
            ),
        ];
        if with_file_system && let Some(file_system) = layout.file_system {
            parts.push(file_system_name(file_system).to_owned());
        }
        parts.join(" · ")
    }

    fn transfer_rate(&self) -> Option<f64> {
        let transfer = self.transfer.as_ref()?;
        let elapsed = transfer.updated.saturating_duration_since(transfer.started);
        if elapsed < MINIMUM_RATE_WINDOW || transfer.done == 0 {
            return None;
        }
        Some(transfer.done as f64 / elapsed.as_secs_f64())
    }

    fn transfer_remaining(&self) -> Option<Duration> {
        let transfer = self.transfer.as_ref()?;
        let rate = self.transfer_rate()?;
        let left = transfer.total.saturating_sub(transfer.done);
        Some(Duration::from_secs_f64(left as f64 / rate))
    }

    pub fn screen(&self, now: Instant) -> Screen {
        match &self.outcome {
            Outcome::Running => self.running_screen(now),
            Outcome::Succeeded {
                finished,
                reboot_at,
            } => self.success_screen(*finished, *reboot_at, now),
            Outcome::Failed {
                finished,
                failure,
                reported,
            } => self.failure_screen(*finished, failure, *reported),
        }
    }

    fn base_screen(&self, tone: Tone, title: String) -> Screen {
        Screen {
            tone,
            product: "EasyDeployMesh",
            device: self.job.device_name.clone(),
            title,
            step_caption: None,
            percent: self.display_percent(),
            percent_label: None,
            remaining_label: None,
            summary: None,
            columns: Vec::new(),
            banner: None,
            footer: None,
        }
    }

    fn running_screen(&self, now: Instant) -> Screen {
        let step = step_number(self.stage);
        let step_label = self.label(self.step_label());
        let remaining = self.transfer_remaining();
        let mut screen = self.base_screen(
            Tone::Running,
            self.verb_title(
                ("正在部署系统", "Deploying Windows"),
                ("正在还原系统", "Restoring Windows"),
            ),
        );
        screen.step_caption = Some(match self.language {
            Language::Chinese => format!("第 {step} 步，共 {TOTAL_STEPS} 步 · {step_label}"),
            Language::English => format!("Step {step} of {TOTAL_STEPS} · {step_label}"),
        });
        screen.percent_label = Some(format!("{}%", screen.percent));
        screen.remaining_label = remaining.map(|left| match self.language {
            Language::Chinese => format!("本步剩余约 {}", format_duration(left)),
            Language::English => format!("About {} left in this step", format_duration(left)),
        });

        let (amount_label, amount) = match &self.transfer {
            Some(transfer) => {
                let label = match transfer.phase {
                    TransferPhase::Download => ("已下载", "Downloaded"),
                    TransferPhase::Verify => ("已校验", "Verified"),
                    TransferPhase::Restore => ("已写入", "Written"),
                };
                (
                    label,
                    format!(
                        "{} / {}",
                        format_bytes(transfer.done.min(transfer.total)),
                        format_bytes(transfer.total)
                    ),
                )
            }
            None => (("已写入", "Written"), "—".to_owned()),
        };
        let left = vec![
            self.row(amount_label.0, amount_label.1, amount),
            self.row(
                "速度",
                "Speed",
                self.transfer_rate()
                    .map(|rate| format!("{}/s", format_bytes(rate as u64)))
                    .unwrap_or_else(|| "—".to_owned()),
            ),
            self.row(
                "已用时间",
                "Elapsed",
                format_duration(now.saturating_duration_since(self.started)),
            ),
            self.row(
                "本步剩余",
                "Step remaining",
                remaining
                    .map(format_duration)
                    .unwrap_or_else(|| "—".to_owned()),
            ),
        ];
        let right = vec![
            self.row("镜像", "Image", self.job.image_name.clone()),
            self.row("目标磁盘", "Disk", self.disk_text()),
            self.row("硬盘型号", "Disk model", self.disk_model_text()),
            self.row("目标分区", "Partition", self.partition_text(true)),
            self.row("控制端", "Server", self.job.control_service.clone()),
        ];
        screen.columns = vec![left, right];
        screen.banner = Some(Banner {
            text: self
                .verb_title(
                    (
                        "部署完成前请不要关机，也不要断开网线。",
                        "Do not turn off this PC or unplug the network cable until deployment finishes.",
                    ),
                    (
                        "还原完成前请不要关机，也不要断开网线。",
                        "Do not turn off this PC or unplug the network cable until the restore finishes.",
                    ),
                ),
            tone: BannerTone::Warning,
        });
        screen
    }

    fn success_screen(&self, finished: Instant, reboot_at: Instant, now: Instant) -> Screen {
        let mut screen = self.base_screen(
            Tone::Success,
            self.verb_title(
                ("部署成功", "Deployment complete"),
                ("还原成功", "Restore complete"),
            ),
        );
        screen.percent = 100;
        screen.summary = Some(
            self.text(
                "系统已写入目标分区并通过校验，引导文件已创建。",
                "Windows was written to the target partition, verified, and made bootable.",
            )
            .to_owned(),
        );
        let written = match self.job.expanded_bytes {
            Some(bytes) => self.row("写入数据", "Data written", format_bytes(bytes)),
            None => self.row("镜像大小", "Image size", format_bytes(self.job.image_bytes)),
        };
        screen.columns = vec![vec![
            self.row("镜像", "Image", self.job.image_name.clone()),
            self.row("目标", "Target", self.target_text(true)),
            written,
            self.row(
                "用时",
                "Duration",
                format_duration(finished.saturating_duration_since(self.started)),
            ),
            Row {
                label: self.text("SHA-256 校验", "SHA-256 check").to_owned(),
                value: self.text("一致", "Match").to_owned(),
                style: ValueStyle::Positive,
                lines: 1,
            },
        ]];
        let left = reboot_at.saturating_duration_since(now);
        let seconds = left.as_secs() + u64::from(left.subsec_nanos() > 0);
        screen.footer = Some(Footer {
            text: if seconds == 0 {
                self.text("正在重启…", "Restarting…").to_owned()
            } else {
                match self.language {
                    Language::Chinese => format!("{seconds} 秒后自动重启进入新系统"),
                    Language::English => {
                        format!("Restarting into the new system in {seconds} s")
                    }
                }
            },
            buttons: vec![
                Button {
                    action: PowerAction::Shutdown,
                    label: self.text("关机", "Shut down").to_owned(),
                    primary: false,
                },
                Button {
                    action: PowerAction::Reboot,
                    label: self.text("立即重启", "Restart now").to_owned(),
                    primary: true,
                },
            ],
        });
        screen
    }

    fn failure_screen(&self, _finished: Instant, failure: &Failure, reported: bool) -> Screen {
        let mut screen = self.base_screen(
            Tone::Failure,
            self.verb_title(
                ("部署失败", "Deployment failed"),
                ("还原失败", "Restore failed"),
            ),
        );
        screen.percent = failure.percent;
        let step_label = self.label(failure.step_label);
        screen.summary = Some(match self.language {
            Language::Chinese => format!(
                "在第 {} 步「{step_label}」停止，已完成 {}%。",
                failure.step, failure.percent
            ),
            Language::English => format!(
                "Stopped at step {} ({step_label}), {}% complete.",
                failure.step, failure.percent
            ),
        });
        screen.columns = vec![vec![
            Row {
                lines: 2,
                ..self.row("原因", "Reason", self.failure_reason(failure))
            },
            Row {
                label: self.text("错误代码", "Error code").to_owned(),
                value: failure_code(failure.step, failure.kind),
                style: ValueStyle::Monospace,
                lines: 1,
            },
            self.row("镜像", "Image", self.job.image_name.clone()),
            self.row("目标", "Target", self.target_text(false)),
            Row {
                label: self.text("日志", "Log").to_owned(),
                value: self.job.log_path.clone(),
                style: ValueStyle::Monospace,
                lines: 1,
            },
        ]];
        screen.banner = Some(match failure.disk_state {
            DiskState::Untouched => Banner {
                text: self
                    .text(
                        "硬盘没有被改动，原来的系统和数据都还在。",
                        "The disk was not changed; the original system and data are still there.",
                    )
                    .to_owned(),
                tone: BannerTone::Neutral,
            },
            DiskState::PartiallyWritten => Banner {
                text: self.verb_title(
                    (
                        "目标分区已被部分写入，这台电脑现在无法从硬盘启动，需要重新部署。",
                        "The disk was partly written and cannot start Windows until it is deployed again.",
                    ),
                    (
                        "目标分区已被部分写入，这台电脑现在无法从硬盘启动，需要重新还原。",
                        "The disk was partly written and cannot start Windows until it is restored again.",
                    ),
                ),
                tone: BannerTone::Danger,
            },
        });
        screen.footer = Some(Footer {
            text: if reported {
                self.text(
                    "错误已上报控制端，可重新下发任务。按 Esc 隐藏本窗口。",
                    "Reported to the control service. Press Esc to hide this window.",
                )
            } else {
                self.text(
                    "正在向控制端上报错误…",
                    "Reporting the error to the control service…",
                )
            }
            .to_owned(),
            buttons: vec![
                Button {
                    action: PowerAction::Shutdown,
                    label: self.text("关机", "Shut down").to_owned(),
                    primary: false,
                },
                Button {
                    action: PowerAction::Reboot,
                    label: self.text("重启", "Restart").to_owned(),
                    primary: true,
                },
            ],
        });
        screen
    }

    fn failure_reason(&self, failure: &Failure) -> String {
        let text = match failure.kind {
            FailureKind::DiskMissing => (
                "目标硬盘已不在这台电脑上",
                "The target disk is no longer present",
            ),
            FailureKind::DiskChanged => (
                "目标硬盘与下发任务时不一致",
                "The target disk changed after the task was confirmed",
            ),
            FailureKind::Checksum => (
                "镜像数据校验不通过，文件可能损坏或不完整",
                "Image verification failed; the file may be damaged or incomplete",
            ),
            FailureKind::Cancelled => (
                "任务已在控制端停止",
                "The task was stopped from the control service",
            ),
            FailureKind::Network => (
                "与控制端的网络连接中断",
                "The network connection to the control service was lost",
            ),
            FailureKind::DiskPart => ("DiskPart 分区失败", "DiskPart could not partition the disk"),
            FailureKind::Dism => ("DISM 写入镜像失败", "DISM could not apply the image"),
            FailureKind::BcdBoot => (
                "BCDBoot 创建引导文件失败",
                "BCDBoot could not create the boot files",
            ),
            FailureKind::Other => return failure.message.clone(),
        };
        self.text(text.0, text.1).to_owned()
    }
}

/// Display step (1-based) for a reported stage. Order matches the executor.
pub fn step_number(stage: DeploymentStage) -> u8 {
    match stage {
        DeploymentStage::Preflight
        | DeploymentStage::BootingInstaller
        | DeploymentStage::ValidatingTarget => 1,
        DeploymentStage::Partitioning => 2,
        DeploymentStage::DownloadingImage => 3,
        DeploymentStage::ApplyingImage | DeploymentStage::InstallingSystem => 4,
        DeploymentStage::ConfiguringBoot
        | DeploymentStage::Finalizing
        | DeploymentStage::AwaitingFirstBoot
        | DeploymentStage::Rebooting => 5,
    }
}

pub fn classify_failure(message: &str) -> FailureKind {
    let message = message.to_ascii_lowercase();
    if message.contains("no longer present") {
        FailureKind::DiskMissing
    } else if message.contains("fingerprint changed") {
        FailureKind::DiskChanged
    } else if message.contains("checksum does not match") || message.contains("sha-256 mismatch") {
        FailureKind::Checksum
    } else if message.contains("deployment control entered unexpected state") {
        FailureKind::Cancelled
    } else if message.starts_with("diskpart.exe failed") {
        FailureKind::DiskPart
    } else if message.starts_with("dism.exe failed") {
        FailureKind::Dism
    } else if message.starts_with("bcdboot.exe failed") {
        FailureKind::BcdBoot
    } else if message.contains("error sending request")
        || message.contains("error decoding response")
        || message.contains("operation timed out")
        || message.contains("connection")
    {
        FailureKind::Network
    } else {
        FailureKind::Other
    }
}

pub fn failure_code(step: u8, kind: FailureKind) -> String {
    let kind = match kind {
        FailureKind::DiskMissing => "DISK-MISSING",
        FailureKind::DiskChanged => "DISK-CHANGED",
        FailureKind::Checksum => "CHECKSUM",
        FailureKind::Cancelled => "STOPPED",
        FailureKind::Network => "NETWORK",
        FailureKind::DiskPart => "DISKPART",
        FailureKind::Dism => "DISM",
        FailureKind::BcdBoot => "BCDBOOT",
        FailureKind::Other => "AGENT",
    };
    format!("EDM-S{step}-{kind}")
}

fn file_system_name(file_system: PartitionFileSystem) -> &'static str {
    match file_system {
        PartitionFileSystem::Ntfs => "NTFS",
        PartitionFileSystem::Fat32 => "FAT32",
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn bounded(text: &str, limit: usize) -> String {
    let mut characters = text.chars();
    let mut result: String = characters.by_ref().take(limit).collect();
    if characters.next().is_some() {
        result.push('…');
    }
    result
}

/// Window-independent rectangle in client pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenLayout {
    pub panel: Rect,
    pub brand_mark: Rect,
    pub product: Rect,
    pub device: Rect,
    /// Horizontal separators across the panel, as one-pixel-high rectangles.
    pub separators: Vec<Rect>,
    pub status_icon: Option<Rect>,
    pub title: Rect,
    pub step_caption: Rect,
    pub bar: Rect,
    pub percent: Rect,
    pub remaining: Rect,
    pub summary: Rect,
    /// Label and value rectangles per column.
    pub rows: Vec<Vec<(Rect, Rect)>>,
    pub column_separator: Option<Rect>,
    pub banner: Option<Rect>,
    pub footer_text: Option<Rect>,
    pub buttons: Vec<(PowerAction, Rect)>,
}

pub const PANEL_MAX_WIDTH: i32 = 800;
pub const PADDING: i32 = 24;
const OUTER_MARGIN: i32 = 16;
const HEADER_HEIGHT: i32 = 64;
const STATUS_HEIGHT: i32 = 130;
const ROW_HEIGHT: i32 = 33;
const ROWS_PADDING: i32 = 16;
const BANNER_HEIGHT: i32 = 44;
const FOOTER_HEIGHT: i32 = 72;
const BUTTON_HEIGHT: i32 = 44;
const BRAND_MARK_SIZE: i32 = 32;

pub fn layout_screen(screen: &Screen, client_width: i32, client_height: i32) -> ScreenLayout {
    let width = (client_width - 2 * OUTER_MARGIN).clamp(1, PANEL_MAX_WIDTH);
    let row_lines = |row: &Row| i32::from(row.lines.clamp(1, 2));
    let line_count = screen
        .columns
        .iter()
        .map(|column| column.iter().map(row_lines).sum::<i32>())
        .max()
        .unwrap_or(0);
    let rows_height = if line_count == 0 {
        0
    } else {
        ROWS_PADDING * 2 + ROW_HEIGHT * line_count
    };
    let height = HEADER_HEIGHT
        + 1
        + STATUS_HEIGHT
        + if rows_height > 0 { 1 + rows_height } else { 0 }
        + screen.banner.as_ref().map_or(0, |_| 1 + BANNER_HEIGHT)
        + screen.footer.as_ref().map_or(0, |_| 1 + FOOTER_HEIGHT);
    let left = (client_width - width) / 2;
    let top = ((client_height - height) / 2).max(0);
    let panel = Rect::new(left, top, left + width, top + height);
    let inner_left = panel.left + PADDING;
    let inner_right = panel.right - PADDING;
    let separator = |y: i32| Rect::new(panel.left, y, panel.right, y + 1);
    let mut layout = ScreenLayout {
        panel,
        ..ScreenLayout::default()
    };

    let mark_top = top + (HEADER_HEIGHT - BRAND_MARK_SIZE) / 2;
    layout.brand_mark = Rect::new(
        inner_left,
        mark_top,
        inner_left + BRAND_MARK_SIZE,
        mark_top + BRAND_MARK_SIZE,
    );
    let product_left = inner_left + BRAND_MARK_SIZE + 12;
    let middle = panel.left + width / 2;
    layout.product = Rect::new(product_left, top, middle, top + HEADER_HEIGHT);
    layout.device = Rect::new(middle, top, inner_right, top + HEADER_HEIGHT);
    let mut y = top + HEADER_HEIGHT;
    layout.separators.push(separator(y));
    y += 1;

    let status_top = y;
    let title_top = status_top + PADDING;
    let title_bottom = title_top + 30;
    if screen.tone == Tone::Running {
        layout.title = Rect::new(inner_left, title_top, middle, title_bottom);
        layout.step_caption = Rect::new(middle - 80, title_top, inner_right, title_bottom);
    } else {
        let icon_top = title_top + 5;
        layout.status_icon = Some(Rect::new(
            inner_left,
            icon_top,
            inner_left + 20,
            icon_top + 20,
        ));
        layout.title = Rect::new(inner_left + 30, title_top, inner_right, title_bottom);
    }
    let bar_top = title_bottom + 14;
    layout.bar = Rect::new(inner_left, bar_top, inner_right, bar_top + 8);
    let caption_top = bar_top + 8 + 12;
    let caption_bottom = caption_top + 22;
    layout.percent = Rect::new(inner_left, caption_top, middle, caption_bottom);
    layout.remaining = Rect::new(middle, caption_top, inner_right, caption_bottom);
    layout.summary = Rect::new(inner_left, caption_top, inner_right, caption_bottom);
    y = status_top + STATUS_HEIGHT;

    if rows_height > 0 {
        layout.separators.push(separator(y));
        y += 1;
        let rows_top = y + ROWS_PADDING;
        let column_count = screen.columns.len().max(1) as i32;
        // Two columns: the narrower left one holds short figures, the right one
        // holds names (image, disk model) that need the room.
        let split = panel.left + width * 9 / 20;
        for column in 0..column_count {
            let (column_left, column_right) = match (column_count, column) {
                (1, _) => (panel.left, panel.right),
                (_, 0) => (panel.left, split),
                _ => (split, panel.right),
            };
            let label_left = column_left + PADDING;
            let value_right = column_right - PADDING;
            let label_right = label_left + if column_count == 1 { 120 } else { 116 };
            let mut row_top = rows_top;
            let rows = screen
                .columns
                .get(column as usize)
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .map(|row| {
                    let row_bottom = row_top + ROW_HEIGHT * row_lines(row);
                    let rects = (
                        Rect::new(label_left, row_top, label_right, row_top + ROW_HEIGHT),
                        Rect::new(label_right + 8, row_top, value_right, row_bottom),
                    );
                    row_top = row_bottom;
                    rects
                })
                .collect();
            layout.rows.push(rows);
            if column > 0 {
                layout.column_separator =
                    Some(Rect::new(column_left, y, column_left + 1, y + rows_height));
            }
        }
        y += rows_height;
    }

    if screen.banner.is_some() {
        layout.separators.push(separator(y));
        y += 1;
        layout.banner = Some(Rect::new(panel.left, y, panel.right, y + BANNER_HEIGHT));
        y += BANNER_HEIGHT;
    }

    if let Some(footer) = &screen.footer {
        layout.separators.push(separator(y));
        y += 1;
        let button_top = y + (FOOTER_HEIGHT - BUTTON_HEIGHT) / 2;
        let mut right = inner_right;
        let mut buttons = Vec::new();
        for button in footer.buttons.iter().rev() {
            let button_width = if button.primary { 104 } else { 80 };
            buttons.push((
                button.action,
                Rect::new(
                    right - button_width,
                    button_top,
                    right,
                    button_top + BUTTON_HEIGHT,
                ),
            ));
            right -= button_width + 8;
        }
        buttons.reverse();
        layout.footer_text = Some(Rect::new(inner_left, y, right - 8, y + FOOTER_HEIGHT));
        layout.buttons = buttons;
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(is_restore: bool) -> JobInfo {
        JobInfo {
            device_name: "LAB-PC-01".to_owned(),
            image_name: "Win11_x64.wim".to_owned(),
            is_restore,
            image_bytes: 6 * 1024 * 1024 * 1024,
            expanded_bytes: None,
            control_service: "192.168.1.10:7760".to_owned(),
            log_path: r"X:\EasyDeployMesh\easydeploymesh-agent.log".to_owned(),
        }
    }

    fn layout_event() -> LocalProgress {
        LocalProgress::Layout(TargetLayout {
            disk_number: 0,
            disk_model: "Virtual Disk".to_owned(),
            windows_partition: 3,
            windows_partition_bytes: 50 * 1024 * 1024 * 1024,
            file_system: Some(PartitionFileSystem::Ntfs),
        })
    }

    #[test]
    fn failure_before_partitioning_reports_an_untouched_disk() {
        let now = Instant::now();
        let mut model = ProgressModel::new(Language::Chinese, job(false), false, now);
        model.stage(DeploymentStage::Preflight, 2);
        model.local(&layout_event(), now);
        model.fail("the selected physical disk is no longer present", now);

        assert_eq!(model.disk_state_after_failure(), Some(DiskState::Untouched));
        let screen = model.screen(now);
        let banner = screen.banner.expect("failure banner");
        assert_eq!(banner.tone, BannerTone::Neutral);
        assert_eq!(banner.text, "硬盘没有被改动，原来的系统和数据都还在。");
        assert_eq!(screen.columns[0][1].value, "EDM-S1-DISK-MISSING");
    }

    #[test]
    fn failure_once_partitioning_is_reported_counts_as_partially_written() {
        let now = Instant::now();
        let mut model = ProgressModel::new(Language::Chinese, job(true), false, now);
        model.stage(DeploymentStage::Preflight, 2);
        model.stage(DeploymentStage::Partitioning, 10);
        model.fail("diskpart.exe failed (exit code: 1)", now);

        assert_eq!(
            model.disk_state_after_failure(),
            Some(DiskState::PartiallyWritten)
        );
        let banner = model.screen(now).banner.expect("failure banner");
        assert_eq!(banner.tone, BannerTone::Danger);
        assert!(banner.text.contains("需要重新还原"));
    }

    #[test]
    fn executor_disk_write_signal_alone_counts_as_partially_written() {
        let now = Instant::now();
        let mut model = ProgressModel::new(Language::English, job(false), false, now);
        model.stage(DeploymentStage::Preflight, 2);
        model.local(&LocalProgress::DiskWriteStarting, now);
        model.fail("anything", now);
        assert_eq!(
            model.disk_state_after_failure(),
            Some(DiskState::PartiallyWritten)
        );
    }

    #[test]
    fn failure_screen_keeps_the_failed_step_and_percent() {
        let start = Instant::now();
        let mut model = ProgressModel::new(Language::Chinese, job(false), false, start);
        model.stage(DeploymentStage::Partitioning, 10);
        model.stage(DeploymentStage::DownloadingImage, 20);
        model.local(
            &LocalProgress::Transfer {
                phase: TransferPhase::Download,
                done: 50,
                total: 100,
            },
            start,
        );
        model.fail(
            "downloaded image checksum does not match the approved image",
            start + Duration::from_secs(5),
        );
        // Stage updates after a failure must not move the frozen failure view.
        model.stage(DeploymentStage::ApplyingImage, 45);
        let screen = model.screen(start + Duration::from_secs(60));

        assert_eq!(screen.tone, Tone::Failure);
        assert_eq!(screen.percent, 30);
        assert_eq!(
            screen.summary.as_deref(),
            Some("在第 3 步「下载镜像」停止，已完成 30%。")
        );
        assert_eq!(
            screen.columns[0][0].value,
            "镜像数据校验不通过，文件可能损坏或不完整"
        );
        assert_eq!(screen.columns[0][1].value, "EDM-S3-CHECKSUM");
        assert_eq!(screen.columns[0][3].value, "—");
    }

    #[test]
    fn running_screen_shows_transfer_rate_remaining_time_and_target() {
        let start = Instant::now();
        let mut model = ProgressModel::new(Language::Chinese, job(false), false, start);
        model.stage(DeploymentStage::Preflight, 2);
        model.local(&layout_event(), start);
        model.stage(DeploymentStage::Partitioning, 10);
        model.stage(DeploymentStage::DownloadingImage, 20);
        let mib = 1024 * 1024;
        model.local(
            &LocalProgress::Transfer {
                phase: TransferPhase::Download,
                done: 0,
                total: 1000 * mib,
            },
            start,
        );
        model.local(
            &LocalProgress::Transfer {
                phase: TransferPhase::Download,
                done: 400 * mib,
                total: 1000 * mib,
            },
            start + Duration::from_secs(4),
        );
        let screen = model.screen(start + Duration::from_secs(65));

        assert_eq!(screen.tone, Tone::Running);
        assert_eq!(screen.title, "正在部署系统");
        assert_eq!(
            screen.step_caption.as_deref(),
            Some("第 3 步，共 5 步 · 下载镜像")
        );
        assert_eq!(screen.percent, 28);
        assert_eq!(screen.remaining_label.as_deref(), Some("本步剩余约 00:06"));
        let left = &screen.columns[0];
        assert_eq!(left[0].label, "已下载");
        assert_eq!(left[0].value, "400.0 MB / 1000.0 MB");
        assert_eq!(left[1].value, "100.0 MB/s");
        assert_eq!(left[2].value, "01:05");
        let right = &screen.columns[1];
        assert_eq!(right[1].value, "磁盘 0");
        assert_eq!(right[2].value, "Virtual Disk");
        assert_eq!(right[3].value, "分区 3 · NTFS · 50.0 GB");
        assert_eq!(
            screen.banner.map(|banner| banner.tone),
            Some(BannerTone::Warning)
        );
    }

    #[test]
    fn display_percent_never_falls_below_the_reported_percent() {
        let now = Instant::now();
        let mut model = ProgressModel::new(Language::English, job(true), false, now);
        model.stage(DeploymentStage::ApplyingImage, 45);
        model.local(
            &LocalProgress::Transfer {
                phase: TransferPhase::Restore,
                done: 1,
                total: 100,
            },
            now,
        );
        assert_eq!(model.display_percent(), 45);
        model.local(
            &LocalProgress::Transfer {
                phase: TransferPhase::Restore,
                done: 100,
                total: 100,
            },
            now,
        );
        assert_eq!(model.display_percent(), 88);
        // A transfer from an earlier stage is dropped when the stage changes.
        model.stage(DeploymentStage::ConfiguringBoot, 88);
        assert_eq!(model.display_percent(), 88);
        assert_eq!(model.screen(now).columns[0][0].value, "—");
    }

    #[test]
    fn success_screen_counts_down_to_restart() {
        let start = Instant::now();
        let mut model = ProgressModel::new(Language::English, job(true), false, start);
        model.local(&layout_event(), start);
        model.stage(DeploymentStage::Rebooting, 99);
        let finished = start + Duration::from_secs(151);
        model.succeed(finished, Duration::from_secs(10));
        assert!(model.is_succeeded());

        let screen = model.screen(finished + Duration::from_millis(500));
        assert_eq!(screen.title, "Restore complete");
        assert_eq!(screen.percent, 100);
        assert_eq!(screen.columns[0][1].value, "Disk 0 · Partition 3 · NTFS");
        assert_eq!(screen.columns[0][3].value, "02:31");
        assert_eq!(screen.columns[0][4].style, ValueStyle::Positive);
        let footer = screen.footer.expect("success footer");
        assert_eq!(footer.text, "Restarting into the new system in 10 s");
        assert_eq!(
            footer
                .buttons
                .iter()
                .map(|button| button.action)
                .collect::<Vec<_>>(),
            vec![PowerAction::Shutdown, PowerAction::Reboot]
        );
        let footer = model
            .screen(finished + Duration::from_secs(11))
            .footer
            .expect("success footer");
        assert_eq!(footer.text, "Restarting…");
    }

    #[test]
    fn failure_reasons_are_classified_from_executor_messages() {
        for (message, kind) in [
            (
                "target disk fingerprint changed after the deployment was confirmed",
                FailureKind::DiskChanged,
            ),
            ("GHO expanded SHA-256 mismatch", FailureKind::Checksum),
            (
                "deployment control entered unexpected state: Cancelled",
                FailureKind::Cancelled,
            ),
            ("dism.exe failed (exit code: 5)", FailureKind::Dism),
            ("bcdboot.exe failed (exit code: 1)", FailureKind::BcdBoot),
            (
                "error sending request for url (http://host/api)",
                FailureKind::Network,
            ),
            ("something unexpected", FailureKind::Other),
        ] {
            assert_eq!(classify_failure(message), kind, "{message}");
        }
        let now = Instant::now();
        let mut model = ProgressModel::new(Language::English, job(false), false, now);
        model.fail(&"x".repeat(500), now);
        let reason = &model.screen(now).columns[0][0].value;
        assert_eq!(reason.chars().count(), REASON_CHARACTER_LIMIT + 1);
        assert!(reason.ends_with('…'));
    }

    #[test]
    fn job_info_comes_from_the_lease_and_strips_the_url_scheme() {
        use easydeploymesh_core::{
            AgentDeploymentImage, AgentGhoDeployment, DeploymentTarget, ImageFormat, PartitionPlan,
        };
        let lease = AgentJobLease {
            job_id: uuid::Uuid::new_v4(),
            lease_id: uuid::Uuid::new_v4(),
            expires_at: chrono::Utc::now(),
            operation: Operation::DeployGho,
            image: AgentDeploymentImage {
                id: uuid::Uuid::new_v4(),
                name: "Win7_x64.gho".to_owned(),
                format: ImageFormat::Gho,
                size_bytes: 8,
                sha256: "00".repeat(32),
                download_url: "/image".to_owned(),
                index: 1,
            },
            target: DeploymentTarget {
                device_id: uuid::Uuid::new_v4(),
                target_disk_id: r"\\.\PhysicalDrive0".to_owned(),
                target_disk_model: String::new(),
                target_disk_serial: None,
                target_disk_size_bytes: 0,
            },
            partition_plan: PartitionPlan::legacy_bios_mbr(),
            gho: Some(AgentGhoDeployment {
                source_partition: 1,
                expanded_size_bytes: 16,
                expanded_sha256: "00".repeat(32),
                parser_version: 1,
            }),
        };
        let job = JobInfo::for_lease(
            &lease,
            "http://192.168.1.10:7760/",
            "MININT-1".to_owned(),
            "agent.log".to_owned(),
        );
        assert_eq!(job.control_service, "192.168.1.10:7760");
        assert!(job.is_restore);
        assert_eq!(job.image_name, "Win7_x64.gho");
        assert_eq!(job.expanded_bytes, Some(16));
    }

    #[test]
    fn formatting_helpers_use_binary_units_and_clock_durations() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(16 * 1024 * 1024 * 1024), "16.0 GB");
        assert_eq!(format_duration(Duration::from_secs(51)), "00:51");
        assert_eq!(format_duration(Duration::from_secs(3_725)), "1:02:05");
    }

    #[test]
    fn an_earlier_job_that_touched_a_disk_rules_out_the_untouched_message() {
        let now = Instant::now();
        let mut first = ProgressModel::new(Language::Chinese, job(false), false, now);
        first.stage(DeploymentStage::Partitioning, 10);
        first.fail("dism.exe failed", now);

        // The retry fails at the fingerprint check, before its own partitioning.
        let mut retry = ProgressModel::new(
            Language::Chinese,
            job(false),
            first.disk_may_be_modified(),
            now,
        );
        retry.stage(DeploymentStage::Preflight, 2);
        retry.fail(
            "target disk fingerprint changed after the deployment was confirmed",
            now,
        );
        assert_eq!(
            retry.disk_state_after_failure(),
            Some(DiskState::PartiallyWritten)
        );
    }

    #[test]
    fn failure_footer_waits_for_the_completion_report() {
        let now = Instant::now();
        let mut model = ProgressModel::new(Language::Chinese, job(false), false, now);
        assert!(!model.is_failed());
        model.fail("dism.exe failed", now);
        assert!(model.is_failed());
        let footer = model.screen(now).footer.expect("failure footer");
        assert_eq!(footer.text, "正在向控制端上报错误…");
        model.failure_reported();
        let footer = model.screen(now).footer.expect("failure footer");
        assert_eq!(
            footer.text,
            "错误已上报控制端，可重新下发任务。按 Esc 隐藏本窗口。"
        );
    }

    /// Conservative width estimate: CJK glyphs at the 14px font size, other
    /// characters at the widest common Latin advance of Segoe UI 14px.
    fn estimated_width(text: &str) -> i32 {
        text.chars()
            .map(|character| if character.is_ascii() { 8 } else { 15 })
            .sum()
    }

    #[test]
    fn fixed_copy_fits_its_area_without_truncation() {
        let now = Instant::now();
        for language in [Language::Chinese, Language::English] {
            for is_restore in [false, true] {
                let mut running = ProgressModel::new(language, job(is_restore), false, now);
                running.local(&layout_event(), now);
                let mut untouched = running.clone();
                untouched.fail("anything", now);
                untouched.failure_reported();
                let mut written = running.clone();
                written.stage(DeploymentStage::Partitioning, 10);
                written.fail("anything", now);
                written.failure_reported();
                let mut succeeded = running.clone();
                succeeded.succeed(now, Duration::from_secs(10));

                for model in [&running, &untouched, &written, &succeeded] {
                    let screen = model.screen(now);
                    let layout = layout_screen(&screen, 1024, 768);
                    if let (Some(banner), Some(area)) = (&screen.banner, layout.banner) {
                        assert!(
                            estimated_width(&banner.text) <= area.width() - 2 * PADDING,
                            "{}",
                            banner.text
                        );
                    }
                    if let (Some(footer), Some(area)) = (&screen.footer, layout.footer_text) {
                        assert!(
                            estimated_width(&footer.text) <= area.width(),
                            "{}",
                            footer.text
                        );
                    }
                    if let Some(summary) = &screen.summary {
                        assert!(
                            estimated_width(summary) <= layout.summary.width(),
                            "{summary}"
                        );
                    }
                    for (column, rects) in screen.columns.iter().zip(&layout.rows) {
                        for (row, (label, _)) in column.iter().zip(rects) {
                            assert!(
                                estimated_width(&row.label) <= label.width(),
                                "{}",
                                row.label
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn long_values_get_room_and_the_reason_may_wrap() {
        let now = Instant::now();
        let mut model = ProgressModel::new(Language::Chinese, job(false), false, now);
        model.local(&layout_event(), now);
        let running = layout_screen(&model.screen(now), 1024, 768);
        // "Windows 11 Pro 23H2 x64 zh-CN.wim" and long disk models.
        assert!(
            running.rows[1][0].1.width() >= estimated_width("Windows 11 Pro 23H2 x64 zh-CN.wim")
        );
        assert!(running.rows[0][0].1.width() >= estimated_width("1000.0 MB / 1000.0 MB"));

        model.fail(&"x".repeat(500), now);
        let screen = model.screen(now);
        assert_eq!(screen.columns[0][0].lines, 2);
        let failed = layout_screen(&screen, 1024, 768);
        let reason = failed.rows[0][0].1;
        assert_eq!(reason.bottom - reason.top, 2 * ROW_HEIGHT);
        assert_eq!(failed.rows[0][1].1.top, reason.bottom);
    }

    #[test]
    fn every_state_fits_the_supported_resolutions() {
        let now = Instant::now();
        let mut running = ProgressModel::new(Language::Chinese, job(false), false, now);
        running.local(&layout_event(), now);
        let mut succeeded = running.clone();
        succeeded.succeed(now, Duration::from_secs(10));
        let mut failed = running.clone();
        failed.stage(DeploymentStage::Partitioning, 10);
        failed.fail("dism.exe failed", now);

        for (width, height) in [(1024, 768), (1280, 800), (1920, 1080)] {
            for model in [&running, &succeeded, &failed] {
                let screen = model.screen(now);
                let layout = layout_screen(&screen, width, height);
                assert!(layout.panel.top >= OUTER_MARGIN, "{width}x{height}");
                assert!(layout.panel.bottom <= height - OUTER_MARGIN);
                assert_eq!(layout.panel.width(), PANEL_MAX_WIDTH);
                let rows = layout.rows.iter().flatten();
                for (label, value) in rows {
                    assert!(label.right < value.left);
                    assert!(value.right <= layout.panel.right - PADDING);
                    assert!(value.bottom <= layout.panel.bottom);
                }
                for (_, button) in &layout.buttons {
                    assert!(button.right <= layout.panel.right - PADDING);
                    assert!(button.bottom <= layout.panel.bottom);
                }
            }
        }
        let failure_layout = layout_screen(&failed.screen(now), 1280, 800);
        assert!(failure_layout.banner.is_some());
        assert_eq!(failure_layout.buttons.len(), 2);
        assert!(failure_layout.buttons[0].1.right < failure_layout.buttons[1].1.left);
        assert!(failure_layout.buttons[1].1.contains(
            failure_layout.buttons[1].1.left + 1,
            failure_layout.buttons[1].1.top + 1
        ));
    }
}
