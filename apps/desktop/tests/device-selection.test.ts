import { describe, expect, it } from 'vitest'
import type { RegisteredDevice } from '../app/types/deployment'
import {
  batchDeploymentTargets,
  deploymentLaunchBlocker,
  deployableDeviceIds,
  deviceSelectionState,
  reconcileDeviceSelection,
  updateDeviceSelection
} from '../app/utils/device-selection'

function device(
  id: string,
  options: { online?: boolean, disks?: number } = {}
): RegisteredDevice {
  const diskCount = options.disks ?? 1
  return {
    device: {
      id,
      hostname: id,
      macAddress: `00:00:00:00:00:${id}`,
      ipAddress: '192.168.1.10',
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
      disks: Array.from({ length: diskCount }, (_, index) => ({
        id: `${id}-disk-${index}`,
        model: 'Disk',
        serial: null,
        sizeBytes: 64_000_000_000,
        isSystem: index === 0
      })),
      lastSeenAt: '2026-08-17T00:00:00Z'
    },
    agentVersion: '0.2.4',
    firstSeenAt: '2026-08-17T00:00:00Z',
    online: options.online ?? true
  }
}

describe('device deployment selection', () => {
  it('only includes online devices that reported a disk', () => {
    const entries = [
      device('ready'),
      device('offline', { online: false }),
      device('diskless', { disks: 0 })
    ]

    expect(deployableDeviceIds(entries)).toEqual(['ready'])
  })

  it('honors operational blockers supplied by the client page', () => {
    const entries = [device('ready'), device('deploying'), device('unknown')]
    const blocked = new Set(['deploying', 'unknown'])
    const available = (entry: RegisteredDevice) =>
      entry.online && entry.device.disks.length > 0 && !blocked.has(entry.device.id)

    expect(deployableDeviceIds(entries, available)).toEqual(['ready'])
    expect(batchDeploymentTargets('all', [], entries, available)
      .map(entry => entry.device.id)).toEqual(['ready'])
    expect(reconcileDeviceSelection(['ready', 'deploying'], entries, available)).toEqual(['ready'])
  })

  it('resolves selected deployment targets without including ineligible devices', () => {
    const entries = [
      device('one'),
      device('two'),
      device('offline', { online: false })
    ]

    expect(batchDeploymentTargets(
      'selected',
      ['two', 'two', 'offline', 'one'],
      entries
    ).map(entry => entry.device.id)).toEqual(['one', 'two'])
  })

  it('resolves all deployment targets independently of the current selection', () => {
    const entries = [
      device('one'),
      device('two'),
      device('offline', { online: false }),
      device('diskless', { disks: 0 })
    ]

    expect(batchDeploymentTargets(
      'all',
      [],
      entries
    ).map(entry => entry.device.id)).toEqual(['one', 'two'])
  })

  it('resolves both batch modes to an empty list when no device is deployable', () => {
    const entries = [
      device('offline', { online: false }),
      device('diskless', { disks: 0 })
    ]

    expect(batchDeploymentTargets('selected', ['offline', 'diskless'], entries)).toEqual([])
    expect(batchDeploymentTargets('all', ['stale'], entries)).toEqual([])
  })

  it('deduplicates duplicate device records before creating batch targets', () => {
    const entries = [device('one'), device('one'), device('two')]

    expect(batchDeploymentTargets('selected', ['one', 'two'], entries)
      .map(entry => entry.device.id)).toEqual(['one', 'two'])
    expect(batchDeploymentTargets('all', [], entries)
      .map(entry => entry.device.id)).toEqual(['one', 'two'])
  })

  it('adds and removes a device without duplicating its id', () => {
    expect(updateDeviceSelection(['one'], 'two', true)).toEqual(['one', 'two'])
    expect(updateDeviceSelection(['one', 'two'], 'one', false)).toEqual(['two'])
    expect(updateDeviceSelection(['one'], 'one', true)).toEqual(['one'])
  })

  it('retains selection across refresh and ordering changes by device id', () => {
    const refreshed = [device('two'), device('one')]
    expect(reconcileDeviceSelection(['one'], refreshed)).toEqual(['one'])
  })

  it('removes devices that go offline, lose their disks, or disappear', () => {
    const refreshed = [
      device('offline', { online: false }),
      device('diskless', { disks: 0 }),
      device('ready')
    ]

    expect(reconcileDeviceSelection(
      ['offline', 'diskless', 'removed', 'ready'],
      refreshed
    )).toEqual(['ready'])
  })

  it('reports unchecked, indeterminate, and checked states', () => {
    const entries = [device('one'), device('two')]

    expect(deviceSelectionState([], entries)).toBe(false)
    expect(deviceSelectionState(['one'], entries)).toBe('indeterminate')
    expect(deviceSelectionState(['one', 'two'], entries)).toBe(true)
  })

  it('reports missing verified images without making an eligible target look unavailable', () => {
    expect(deploymentLaunchBlocker(1, 0)).toBe('images')
    expect(deploymentLaunchBlocker(1, 1)).toBeNull()
    expect(deploymentLaunchBlocker(0, 1)).toBe('targets')
  })
})
