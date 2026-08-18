import type { RegisteredDevice } from '~/types/deployment'

export function templateDiskGroups(
  targets: readonly RegisteredDevice[],
  selectedDiskIds: Readonly<Record<string, string>>
) {
  return targets.map(entry => ({
    deviceId: entry.device.id,
    deviceName: entry.device.hostname || entry.device.macAddress,
    ipAddress: entry.device.ipAddress,
    macAddress: entry.device.macAddress,
    disks: entry.device.disks.map(disk => ({
      key: `${entry.device.id}:${disk.id}`,
      disk,
      selected: selectedDiskIds[entry.device.id] === disk.id
    }))
  }))
}
