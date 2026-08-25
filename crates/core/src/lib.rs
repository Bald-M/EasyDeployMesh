//! Domain types shared by the EasyDeployMesh desktop service and WinPE agent.

mod job;
mod models;

pub use job::{
    AgentDeploymentImage, AgentGhoDeployment, AgentJobCompletion, AgentJobLease, AgentJobProgress,
    CreateDeploymentJob, DeploymentJob, DeploymentOptions, DeploymentStage, DeploymentTarget,
    IMAGE_CACHE_HEADROOM_MIB, JobState, JobTransitionError, MIB_BYTES, MINIMUM_DATA_MIB,
    MINIMUM_WINDOWS_MIB, PARTITION_ALIGNMENT_HEADROOM_MIB, PartitionCapacityError,
    PartitionFileSystem, PartitionPlan, PartitionPlanError, PartitionRole, PartitionSpec,
    PartitionTable, WindowsPayloadCapacityError,
};
pub use models::{
    ActivityEvent, ActivitySeverity, ActivitySource, ActivitySubject, AgentHeartbeat,
    AgentHeartbeatAck, AgentInventory, AgentRegistration, Architecture, BootMode,
    ControlPlaneStatus, Device, Disk, GhoImageCapability, GhoPartitionCapability, ImageArtifact,
    ImageFormat, Operation, PxeClientStage, PxeConfig, PxeDiscoveredClient, PxeEvent, PxeMode,
    PxeServiceStatus, RegisteredDevice, RuntimeStatus, SystemDetails,
};
