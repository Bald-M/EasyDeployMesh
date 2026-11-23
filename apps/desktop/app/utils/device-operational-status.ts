import type { DeploymentJob, RegisteredDevice } from '~/types/deployment'

export type DeviceOperationalStatus = 'online' | 'offline' | 'deploying' | 'paused' | 'unknown'

function activeDeploymentFor(
  deviceId: string,
  jobs: readonly DeploymentJob[]
) {
  return jobs.find(job =>
    (job.state === 'running' || job.state === 'paused')
    && job.targets.some(target => target.deviceId === deviceId)
  )
}

function hasValidLease(job: DeploymentJob, now: Date) {
  if (!job.leaseId || !job.leaseExpiresAt) return false
  const expiresAt = new Date(job.leaseExpiresAt).getTime()
  return Number.isFinite(expiresAt) && expiresAt > now.getTime()
}

export function deviceOperationalStatus(
  entry: RegisteredDevice,
  jobs: readonly DeploymentJob[],
  now = new Date()
): DeviceOperationalStatus {
  const activeDeployment = activeDeploymentFor(entry.device.id, jobs)
  if (!activeDeployment) return entry.online ? 'online' : 'offline'
  if (!hasValidLease(activeDeployment, now)) return 'unknown'
  return activeDeployment.state === 'paused' ? 'paused' : 'deploying'
}

// One pass over the jobs for every device, for lists that render many rows.
export function deviceOperationalStatuses(
  entries: readonly RegisteredDevice[],
  jobs: readonly DeploymentJob[],
  now = new Date()
): Map<string, DeviceOperationalStatus> {
  const activeByDevice = new Map<string, DeploymentJob>()
  for (const job of jobs) {
    if (job.state !== 'running' && job.state !== 'paused') continue
    for (const target of job.targets) {
      if (!activeByDevice.has(target.deviceId)) activeByDevice.set(target.deviceId, job)
    }
  }
  return new Map(entries.map((entry) => {
    const active = activeByDevice.get(entry.device.id)
    const status: DeviceOperationalStatus = !active
      ? (entry.online ? 'online' : 'offline')
      : !hasValidLease(active, now)
          ? 'unknown'
          : active.state === 'paused' ? 'paused' : 'deploying'
    return [entry.device.id, status]
  }))
}

export function isDeviceDeploymentAvailable(
  entry: RegisteredDevice,
  jobs: readonly DeploymentJob[],
  now = new Date()
) {
  return deviceOperationalStatus(entry, jobs, now) === 'online'
    && entry.device.disks.some(disk => !disk.isSystem)
}
