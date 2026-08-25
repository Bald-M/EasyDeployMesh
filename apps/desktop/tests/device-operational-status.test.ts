import { describe, expect, it } from 'vitest'
import type { DeploymentJob, RegisteredDevice } from '../app/types/deployment'
import {
  deviceOperationalStatus,
  isDeviceDeploymentAvailable
} from '../app/utils/device-operational-status'

const now = new Date('2026-08-25T05:00:00Z')

function device(online: boolean): RegisteredDevice {
  return {
    device: {
      id: 'device-1',
      hostname: 'client-1',
      macAddress: '00:00:00:00:00:01',
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
      disks: [{
        id: 'disk-1',
        model: 'Disk',
        serial: null,
        sizeBytes: 64_000_000_000,
        isSystem: true
      }],
      lastSeenAt: '2026-08-25T04:59:00Z'
    },
    agentVersion: '0.2.4',
    firstSeenAt: '2026-08-25T04:00:00Z',
    online
  }
}

function job(
  state: DeploymentJob['state'],
  leaseExpiresAt: string | null = '2026-08-25T06:00:00Z'
): DeploymentJob {
  return {
    id: `job-${state}`,
    name: 'Deployment',
    operation: 'deploy_wim',
    imageId: 'image-1',
    targets: [{
      deviceId: 'device-1',
      targetDiskId: 'disk-1',
      targetDiskModel: 'Disk',
      targetDiskSerial: null,
      targetDiskSizeBytes: 64_000_000_000
    }],
    options: { imageIndex: 1, partitionPlan: { table: 'gpt', partitions: [] } },
    state,
    stage: null,
    progressPercent: 20,
    statusMessage: null,
    errorMessage: null,
    leaseId: leaseExpiresAt ? 'lease-1' : null,
    leaseExpiresAt,
    createdAt: '2026-08-25T04:00:00Z',
    updatedAt: '2026-08-25T04:59:00Z'
  }
}

describe('device operational status', () => {
  it('shows an offline client with a valid running lease as deploying', () => {
    expect(deviceOperationalStatus(device(false), [job('running')], now)).toBe('deploying')
  })

  it('shows running clients with missing, expired, or invalid leases as unknown', () => {
    expect(deviceOperationalStatus(device(false), [job('running', null)], now)).toBe('unknown')
    expect(deviceOperationalStatus(device(false), [job('running', '2026-08-25T04:59:59Z')], now)).toBe('unknown')
    expect(deviceOperationalStatus(device(false), [job('running', 'not-a-date')], now)).toBe('unknown')
  })

  it('distinguishes paused deployments with a valid lease', () => {
    expect(deviceOperationalStatus(device(true), [job('paused')], now)).toBe('paused')
  })

  it('does not let waiting or terminal jobs override normal presence', () => {
    expect(deviceOperationalStatus(device(true), [job('waiting')], now)).toBe('online')
    expect(deviceOperationalStatus(device(false), [job('succeeded')], now)).toBe('offline')
  })

  it('only allows ordinary online clients without an active deployment', () => {
    expect(isDeviceDeploymentAvailable(device(true), [], now)).toBe(true)
    expect(isDeviceDeploymentAvailable(device(true), [job('running')], now)).toBe(false)
    expect(isDeviceDeploymentAvailable(device(true), [job('paused')], now)).toBe(false)
    expect(isDeviceDeploymentAvailable(device(false), [], now)).toBe(false)
  })
})
