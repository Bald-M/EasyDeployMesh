export type ImageFormat = 'gho' | 'wim' | 'esd' | 'swm'

export interface ImageArtifact {
  id: string
  name: string
  format: ImageFormat
  sourcePath: string
  sizeBytes: number
  sha256: string | null
  spans: string[]
  verified: boolean
  ghoCapability?: GhoImageCapability | null
  createdAt: string
}

export interface GhoImageCapability {
  deployable: boolean
  compression: string | null
  expandedSizeBytes: number | null
  expandedSha256: string | null
  partitionCount: number | null
  sourcePartition: number | null
  partitions: GhoPartitionCapability[]
  parserVersion: number
  blockedReason: string | null
}

export interface GhoPartitionCapability {
  sourcePartition: number
  fileSystem: string
  expandedSizeBytes: number
  expandedSha256: string
}


export type Operation = 'deploy_gho' | 'capture_gho' | 'deploy_wim'
export type JobState =
  | 'draft'
  | 'waiting'
  | 'running'
  | 'paused'
  | 'succeeded'
  | 'failed'
  | 'cancelled'

export interface DeploymentTarget {
  deviceId: string
  targetDiskId: string
  targetDiskModel: string
  targetDiskSerial: string | null
  targetDiskSizeBytes: number
}

export type PartitionTable = 'gpt' | 'mbr'
export type PartitionRole = 'efi' | 'msr' | 'system' | 'windows' | 'recovery' | 'data'
export type PartitionFileSystem = 'fat32' | 'ntfs'

export interface PartitionSpec {
  role: PartitionRole
  sizeMib: number | null
  fileSystem: PartitionFileSystem | null
  label: string
  driveLetter?: string | null
}

export interface PartitionPlan {
  table: PartitionTable
  partitions: PartitionSpec[]
}

export interface DeploymentOptions {
  imageIndex: number
  partitionPlan: PartitionPlan
}

export interface DeploymentJob {
  id: string
  name: string
  operation: Operation
  imageId: string
  targets: DeploymentTarget[]
  options: DeploymentOptions
  state: JobState
  stage: DeploymentStage | null
  progressPercent: number
  statusMessage: string | null
  errorMessage: string | null
  leaseId: string | null
  leaseExpiresAt: string | null
  createdAt: string
  updatedAt: string
}

export interface CreateDeploymentJob {
  name: string
  operation: Operation
  imageId: string
  targets: DeploymentTarget[]
  options: DeploymentOptions
}

export type DeploymentStage =
  | 'preflight'
  | 'partitioning'
  | 'downloading_image'
  | 'applying_image'
  | 'configuring_boot'
  | 'finalizing'
  | 'rebooting'

export type Architecture = 'x86_64' | 'aarch64' | 'unknown'
export type BootMode = 'uefi' | 'legacy_bios' | 'unknown'

export interface Disk {
  id: string
  model: string
  serial: string | null
  sizeBytes: number
  isSystem: boolean
}

export interface MemoryModule {
  manufacturer: string | null
  partNumber: string | null
  capacityBytes: number
  speedMhz: number | null
}

export interface HardwareComponent {
  name: string
  manufacturer: string | null
  memoryBytes: number | null
}

export interface NetworkAdapter {
  name: string
  manufacturer: string | null
  macAddress: string | null
  speedBps: number | null
}

export interface SystemDetails {
  osName: string | null
  osVersion: string | null
  uptimeSeconds: number | null
  motherboard: string | null
  memoryModules: MemoryModule[]
  gpus: HardwareComponent[]
  displays: HardwareComponent[]
  audioDevices: HardwareComponent[]
  networkAdapters: NetworkAdapter[]
}

export interface Device {
  id: string
  hostname: string | null
  macAddress: string
  ipAddress: string
  model: string | null
  serial: string | null
  cpuModel: string | null
  physicalCoreCount: number | null
  logicalProcessorCount: number
  memoryBytes: number
  systemDetails: SystemDetails
  architecture: Architecture
  bootMode: BootMode
  disks: Disk[]
  lastSeenAt: string
}

export interface RegisteredDevice {
  device: Device
  agentVersion: string
  firstSeenAt: string
  online: boolean
}
