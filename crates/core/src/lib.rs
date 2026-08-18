//! Domain types shared by the EasyDeployMesh desktop service and WinPE agent.

mod job;
mod models;

pub use job::{
    AgentDeploymentImage, AgentGhoDeployment, AgentJobCompletion, AgentJobLease, AgentJobProgress,
    CreateDeploymentJob, DeploymentJob, DeploymentOptions, DeploymentStage, DeploymentTarget,
    JobState, JobTransitionError, PartitionFileSystem, PartitionPlan, PartitionPlanError,
    PartitionRole, PartitionSpec, PartitionTable,
};
pub use models::{
    ActivityEvent, ActivitySeverity, ActivitySource, ActivitySubject, AgentHeartbeat,
    AgentHeartbeatAck, AgentInventory, AgentRegistration, Architecture, BootMode,
    ControlPlaneStatus, Device, Disk, GhoImageCapability, ImageArtifact, ImageFormat, Operation,
    PxeClientStage, PxeConfig, PxeDiscoveredClient, PxeEvent, PxeMode, PxeServiceStatus,
    RegisteredDevice, RuntimeStatus, SystemDetails,
};
