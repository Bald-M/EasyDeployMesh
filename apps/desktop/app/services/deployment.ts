import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import type {
  CreateDeploymentJob,
  DeploymentJob,
  ImageArtifact,
  RegisteredDevice
} from '~/types/deployment'
import { isTauriRuntime } from '~/services/runtime'

export async function pickImageFiles(): Promise<string[]> {
  if (!isTauriRuntime()) {
    return []
  }

  const selection = await open({
    multiple: true,
    directory: false,
    filters: [
      {
        name: 'Windows deployment images',
        extensions: ['gho', 'wim', 'esd', 'swm']
      }
    ]
  })

  if (!selection) {
    return []
  }

  return Array.isArray(selection) ? selection : [selection]
}

export function verifyGhoImage(id: string): Promise<ImageArtifact> {
  return invoke<ImageArtifact>('verify_gho_image', { id })
}

export async function getImages(): Promise<ImageArtifact[]> {
  if (!isTauriRuntime()) {
    return []
  }

  return invoke<ImageArtifact[]>('list_images')
}

export async function importImage(path: string): Promise<ImageArtifact> {
  return invoke<ImageArtifact>('import_image', { path })
}

export async function removeImage(id: string): Promise<boolean> {
  return invoke<boolean>('remove_image', { id })
}

export async function getJobs(): Promise<DeploymentJob[]> {
  if (!isTauriRuntime()) {
    return []
  }

  return invoke<DeploymentJob[]>('list_jobs')
}

export async function createJob(request: CreateDeploymentJob): Promise<DeploymentJob> {
  return invoke<DeploymentJob>('create_job', { request })
}

export async function removeJob(id: string): Promise<boolean> {
  return invoke<boolean>('remove_job', { id })
}

export async function transitionJob(id: string, nextState: DeploymentJob['state']): Promise<DeploymentJob> {
  return invoke<DeploymentJob>('transition_job', { id, nextState })
}

export async function getDevices(verifyOnline = false): Promise<RegisteredDevice[]> {
  if (!isTauriRuntime()) {
    return []
  }

  return invoke<RegisteredDevice[]>(verifyOnline ? 'refresh_devices' : 'list_devices')
}

export async function removeDevice(id: string): Promise<boolean> {
  return invoke<boolean>('remove_device', { id })
}
