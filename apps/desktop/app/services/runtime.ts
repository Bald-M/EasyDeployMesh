import { invoke } from '@tauri-apps/api/core'
import type {
  ControlPlaneStatus,
  BootPackage,
  NetworkInterfaceSummary,
  PxeConfig,
  PxeDiscoveredClient,
  PxeServiceStatus,
  RuntimeStatus
} from '~/types/runtime'
import type { WinpeImportCapability } from '~/types/runtime'
import type { ActivityEvent, ActivityQuery } from '~/types/runtime'

const browserRuntime: RuntimeStatus = {
  serviceState: 'idle',
  version: '0.2.6',
  platform: 'browser',
  activeInterface: null,
  connectedDevices: 0,
  queuedJobs: 0
}

const browserInterfaces: NetworkInterfaceSummary[] = [
  {
    name: 'en0',
    address: '192.168.1.24',
    netmask: '255.255.255.0',
    isUp: true,
    isLoopback: false
  },
  {
    name: 'lo0',
    address: '127.0.0.1',
    netmask: '255.0.0.0',
    isUp: true,
    isLoopback: true
  }
]

const browserControlStatus: ControlPlaneStatus = {
  state: 'idle',
  bindAddress: null,
  port: null,
  endpoint: null,
  enrollmentToken: null
}

export function isTauriRuntime(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

export async function getRuntimeStatus(): Promise<RuntimeStatus> {
  if (!isTauriRuntime()) {
    return browserRuntime
  }

  return invoke<RuntimeStatus>('runtime_status')
}

export async function getWinpeImportCapability(): Promise<WinpeImportCapability> {
  if (!isTauriRuntime()) {
    return { supported: false, backend: null, reason: 'native_runtime_required', version: null }
  }
  return invoke<WinpeImportCapability>('winpe_import_capability')
}

export async function getNetworkInterfaces(): Promise<NetworkInterfaceSummary[]> {
  if (!isTauriRuntime()) {
    return browserInterfaces
  }

  return invoke<NetworkInterfaceSummary[]>('network_interfaces')
}

export async function getControlPlaneStatus(): Promise<ControlPlaneStatus> {
  if (!isTauriRuntime()) {
    return browserControlStatus
  }

  return invoke<ControlPlaneStatus>('control_plane_status')
}

export async function startControlPlane(
  bindAddress: string,
  port = 7760
): Promise<ControlPlaneStatus> {
  return invoke<ControlPlaneStatus>('start_control_plane', {
    bindAddress,
    port
  })
}

export async function stopControlPlane(): Promise<ControlPlaneStatus> {
  return invoke<ControlPlaneStatus>('stop_control_plane')
}

export async function loadPxeConfig(): Promise<PxeConfig | null> {
  if (!isTauriRuntime()) return null
  return invoke<PxeConfig | null>('load_pxe_config')
}

export function savePxeConfig(config: PxeConfig): Promise<PxeConfig> {
  return invoke<PxeConfig>('save_pxe_config', { config })
}

export function importPxeBootPackage(source: string, biosBootFile: string, uefiX64BootFile: string): Promise<BootPackage> {
  return invoke<BootPackage>('import_pxe_boot_package', { source, biosBootFile, uefiX64BootFile })
}

export function importPxeMedia(source: string): Promise<BootPackage> {
  return invoke<BootPackage>('import_pxe_media', { source })
}

export async function getPxeServiceStatus(): Promise<PxeServiceStatus> {
  if (!isTauriRuntime()) return { state: 'idle', mode: null, bindAddress: null, dhcpPort: null, proxyDhcpPort: null, tftpPort: null, activeLeases: 0, lastError: null }
  return invoke<PxeServiceStatus>('pxe_service_status')
}

export function startPxeService(config: PxeConfig, controlPort = 7760): Promise<PxeServiceStatus> {
  return invoke<PxeServiceStatus>('start_pxe_service', { config, controlPort })
}

export function stopPxeService(): Promise<PxeServiceStatus> {
  return invoke<PxeServiceStatus>('stop_pxe_service')
}

export async function getPxeDiscoveredClients(): Promise<PxeDiscoveredClient[]> {
  if (!isTauriRuntime()) return []
  return invoke<PxeDiscoveredClient[]>('pxe_discovered_clients')
}

export async function getActivityEvents(query: ActivityQuery = {}): Promise<ActivityEvent[]> {
  if (!isTauriRuntime()) return []
  return invoke<ActivityEvent[]>('activity_events', { query })
}
