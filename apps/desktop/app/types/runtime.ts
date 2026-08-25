export type ServiceState = 'idle' | 'starting' | 'running' | 'stopping' | 'error'

export interface RuntimeStatus {
  serviceState: ServiceState
  version: string
  platform: string
  activeInterface: string | null
  connectedDevices: number
  queuedJobs: number
}

export interface NetworkInterfaceSummary {
  name: string
  address: string
  netmask: string
  isLoopback: boolean
  isUp: boolean
}

export interface ControlPlaneStatus {
  state: ServiceState
  bindAddress: string | null
  port: number | null
  endpoint: string | null
  enrollmentToken: string | null
}

export type PxeMode = 'standalone_dhcp' | 'proxy_dhcp'
export type PxeClientStage = 'discovered' | 'downloading' | 'waiting_for_agent'

export interface PxeConfig {
  mode: PxeMode
  bindAddress: string
  subnetMask: string
  poolStart: string
  poolEnd: string
  leaseSeconds: number
  gateway: string | null
  dnsServers: string[]
  tftpRoot: string
  biosBootFile: string
  uefiX64BootFile: string
}

export interface BootPackage {
  root: string
  biosBootFile: string
  uefiX64BootFile: string
}

export interface WinpeImportCapability {
  supported: boolean
  backend: 'windows_native' | 'wimlib' | null
  reason: string | null
  version: string | null
}

export interface PxeServiceStatus {
  state: ServiceState
  mode: PxeMode | null
  bindAddress: string | null
  dhcpPort: number | null
  proxyDhcpPort: number | null
  tftpPort: number | null
  activeLeases: number
  lastError: string | null
}

export interface PxeDiscoveredClient {
  macAddress: string
  ipAddress: string | null
  architecture: 'x86_64' | 'aarch64' | 'unknown'
  stage: PxeClientStage
  firstSeenAt: string
  lastSeenAt: string
}

export type ActivitySource = 'service' | 'device' | 'deployment'
export type ActivitySeverity = 'info' | 'success' | 'warning' | 'error'

export interface ActivitySubject {
  id: string
  name: string
}

export interface ActivityEvent {
  id: string
  occurredAt: string
  source: ActivitySource
  kind: string
  severity: ActivitySeverity
  subject: ActivitySubject | null
  details: Record<string, unknown>
  rawMessage: string | null
}

export interface ActivityQuery {
  sources?: ActivitySource[]
  severities?: ActivitySeverity[]
  before?: string
  after?: string
  limit?: number
}
