import type { RegisteredDevice } from '~/types/deployment'

export type DeviceSelectionState = boolean | 'indeterminate'
export type BatchSendMode = 'selected' | 'all'
export type DeploymentLaunchBlocker = 'targets' | 'images'

export function deploymentLaunchBlocker(
  targetCount: number,
  verifiedImageCount: number
): DeploymentLaunchBlocker | null {
  if (targetCount === 0) return 'targets'
  if (verifiedImageCount === 0) return 'images'
  return null
}

export function isDeployableDevice(entry: RegisteredDevice) {
  return entry.online && entry.device.disks.length > 0
}

export function deployableDeviceIds(entries: readonly RegisteredDevice[]) {
  return entries
    .filter(isDeployableDevice)
    .map(entry => entry.device.id)
}

export function batchDeploymentTargets(
  mode: BatchSendMode,
  selectedIds: readonly string[],
  entries: readonly RegisteredDevice[]
) {
  const selected = new Set(selectedIds)
  const seen = new Set<string>()
  return entries.filter((entry) => {
    const deviceId = entry.device.id
    if (!isDeployableDevice(entry) || seen.has(deviceId)) return false
    if (mode === 'selected' && !selected.has(deviceId)) return false
    seen.add(deviceId)
    return true
  })
}

export function reconcileDeviceSelection(
  selectedIds: readonly string[],
  entries: readonly RegisteredDevice[]
) {
  const deployableIds = new Set(deployableDeviceIds(entries))
  return [...new Set(selectedIds)].filter(id => deployableIds.has(id))
}

export function updateDeviceSelection(
  selectedIds: readonly string[],
  deviceId: string,
  selected: boolean
) {
  const next = new Set(selectedIds)
  if (selected) {
    next.add(deviceId)
  } else {
    next.delete(deviceId)
  }
  return [...next]
}

export function deviceSelectionState(
  selectedIds: readonly string[],
  entries: readonly RegisteredDevice[]
): DeviceSelectionState {
  const deployableIds = deployableDeviceIds(entries)
  if (deployableIds.length === 0) return false

  const selected = new Set(selectedIds)
  const selectedDeployableCount = deployableIds.filter(id => selected.has(id)).length
  if (selectedDeployableCount === 0) return false
  if (selectedDeployableCount === deployableIds.length) return true
  return 'indeterminate'
}
