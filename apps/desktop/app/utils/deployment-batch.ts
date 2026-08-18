import type { CreateDeploymentJob, DeploymentTarget } from '~/types/deployment'

export type SingleTargetDeployment = Omit<CreateDeploymentJob, 'targets'> & {
  target: DeploymentTarget
}

export interface DeploymentBatchProgress {
  completed: number
  total: number
}

export interface DeploymentBatchResult {
  queuedDeviceIds: string[]
  failedDeviceIds: string[]
  pendingDeviceIds: string[]
  interrupted: boolean
}

interface DeploymentBatchOptions {
  confirmationIsValid: () => boolean
  enqueue: (request: CreateDeploymentJob) => Promise<unknown>
  onProgress?: (progress: DeploymentBatchProgress) => void
}

export async function enqueueDeploymentBatch(
  deployments: readonly SingleTargetDeployment[],
  options: DeploymentBatchOptions
): Promise<DeploymentBatchResult> {
  const deviceIds = deployments.map(deployment => deployment.target.deviceId)
  if (new Set(deviceIds).size !== deviceIds.length) {
    throw new Error('a deployment batch cannot contain duplicate device targets')
  }

  const queuedDeviceIds: string[] = []
  const failedDeviceIds: string[] = []

  for (let index = 0; index < deployments.length; index += 1) {
    if (!options.confirmationIsValid()) {
      return {
        queuedDeviceIds,
        failedDeviceIds,
        pendingDeviceIds: deviceIds.slice(index),
        interrupted: true
      }
    }

    const { target, ...request } = deployments[index]!
    const deviceId = deviceIds[index]!
    try {
      await options.enqueue({ ...request, targets: [target] })
      queuedDeviceIds.push(deviceId)
    } catch {
      failedDeviceIds.push(deviceId)
    } finally {
      options.onProgress?.({ completed: index + 1, total: deployments.length })
    }
  }

  return {
    queuedDeviceIds,
    failedDeviceIds,
    pendingDeviceIds: [],
    interrupted: false
  }
}
