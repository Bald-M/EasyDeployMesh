import { describe, expect, it, vi } from 'vitest'
import { enqueueDeploymentBatch, type SingleTargetDeployment } from '../app/utils/deployment-batch'

function request(deviceId: string): SingleTargetDeployment {
  return {
    name: `Deploy to ${deviceId}`,
    operation: 'deploy_wim',
    imageId: 'image-one',
    target: {
      deviceId,
      targetDiskId: `${deviceId}-disk`,
      targetDiskModel: 'Test disk',
      targetDiskSerial: null,
      targetDiskSizeBytes: 64_000_000_000
    },
    options: {
      imageIndex: 1,
      partitionPlan: {
        table: 'gpt',
        partitions: [{
          role: 'windows',
          sizeMib: null,
          fileSystem: 'ntfs',
          label: 'Windows',
          driveLetter: 'C'
        }]
      }
    }
  }
}

describe('deployment batch orchestration', () => {
  it('queues one single-target job per device and reports progress', async () => {
    const enqueue = vi.fn().mockResolvedValue(undefined)
    const onProgress = vi.fn()
    const result = await enqueueDeploymentBatch(
      [request('one'), request('two')],
      { confirmationIsValid: () => true, enqueue, onProgress }
    )

    expect(enqueue).toHaveBeenCalledTimes(2)
    expect(enqueue.mock.calls.map(([job]) => job.targets.map(target => target.deviceId)))
      .toEqual([['one'], ['two']])
    expect(onProgress.mock.calls.map(([progress]) => progress)).toEqual([
      { completed: 1, total: 2 },
      { completed: 2, total: 2 }
    ])
    expect(result).toEqual({
      queuedDeviceIds: ['one', 'two'],
      failedDeviceIds: [],
      pendingDeviceIds: [],
      interrupted: false
    })
  })

  it('continues after an enqueue failure and returns only the failed device', async () => {
    const enqueue = vi.fn()
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error('conflict'))
      .mockResolvedValueOnce(undefined)

    await expect(enqueueDeploymentBatch(
      [request('one'), request('two'), request('three')],
      { confirmationIsValid: () => true, enqueue }
    )).resolves.toEqual({
      queuedDeviceIds: ['one', 'three'],
      failedDeviceIds: ['two'],
      pendingDeviceIds: [],
      interrupted: false
    })
  })

  it('stops before the next job when the destructive confirmation changes', async () => {
    let confirmed = true
    const enqueue = vi.fn().mockImplementation(async () => {
      confirmed = false
    })

    await expect(enqueueDeploymentBatch(
      [request('one'), request('two'), request('three')],
      { confirmationIsValid: () => confirmed, enqueue }
    )).resolves.toEqual({
      queuedDeviceIds: ['one'],
      failedDeviceIds: [],
      pendingDeviceIds: ['two', 'three'],
      interrupted: true
    })
    expect(enqueue).toHaveBeenCalledTimes(1)
  })

  it('rejects duplicate targets before enqueuing anything', async () => {
    const enqueue = vi.fn().mockResolvedValue(undefined)
    await expect(enqueueDeploymentBatch(
      [request('one'), request('one')],
      { confirmationIsValid: () => true, enqueue }
    )).rejects.toThrow('duplicate device targets')
    expect(enqueue).not.toHaveBeenCalled()
  })
})
