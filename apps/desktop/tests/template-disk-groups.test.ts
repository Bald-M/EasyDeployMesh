import { describe, expect, it } from 'vitest'
import type { RegisteredDevice } from '../app/types/deployment'
import { templateDiskGroups } from '../app/utils/template-disk-groups'

function device(id: string, hostname: string, diskIds: string[]): RegisteredDevice {
  return {
    device: {
      id,
      hostname,
      macAddress: `00:00:00:00:00:0${id}`,
      ipAddress: `192.168.1.${id}`,
      model: null,
      serial: null,
      cpuModel: null,
      physicalCoreCount: null,
      logicalProcessorCount: 0,
      memoryBytes: 0,
      systemDetails: {
        osName: null,
        osVersion: null,
        uptimeSeconds: null,
        motherboard: null,
        memoryModules: [],
        gpus: [],
        displays: [],
        audioDevices: [],
        networkAdapters: []
      },
      architecture: 'x86_64',
      bootMode: 'uefi',
      disks: diskIds.map(diskId => ({
        id: diskId,
        model: 'VMware Virtual Disk',
        serial: null,
        sizeBytes: 60 * 1024 ** 3,
        isSystem: false
      })),
      lastSeenAt: '2026-08-17T00:00:00Z'
    },
    agentVersion: '0.2.4',
    firstSeenAt: '2026-08-17T00:00:00Z',
    online: true
  }
}

describe('partition-template target disk grouping', () => {
  it('keeps disks grouped under their owning device in a batch', () => {
    const groups = templateDiskGroups(
      [device('1', 'vm-one', ['disk-a']), device('2', 'vm-two', ['disk-b'])],
      { '1': 'disk-a', '2': 'disk-b' }
    )

    expect(groups.map(group => ({
      deviceId: group.deviceId,
      deviceName: group.deviceName,
      diskIds: group.disks.map(row => row.disk.id),
      selectedDiskIds: group.disks.filter(row => row.selected).map(row => row.disk.id)
    }))).toEqual([
      { deviceId: '1', deviceName: 'vm-one', diskIds: ['disk-a'], selectedDiskIds: ['disk-a'] },
      { deviceId: '2', deviceName: 'vm-two', diskIds: ['disk-b'], selectedDiskIds: ['disk-b'] }
    ])
  })

  it('marks at most one selected target disk inside each device group', () => {
    const groups = templateDiskGroups(
      [device('1', 'vm-one', ['disk-a', 'disk-b'])],
      { '1': 'disk-b' }
    )

    expect(groups[0]?.disks.map(row => [row.disk.id, row.selected])).toEqual([
      ['disk-a', false],
      ['disk-b', true]
    ])
  })
})
