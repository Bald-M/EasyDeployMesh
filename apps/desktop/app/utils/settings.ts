import type { PxeMode } from '~/types/runtime'

export interface SettingsDraft {
  bindAddress: string
  port: number
  pxeMode: PxeMode
  subnetMask: string
  poolStart: string
  poolEnd: string
  leaseSeconds: number
  gateway: string
  dnsServers: string
  tftpRoot: string
  peName: string
  biosBootFile: string
  uefiX64BootFile: string
}

export const settingsDraftStorageKey = 'easydeploymesh.settings.draft.v1'

export function parseSettingsDraft(value: string | null): SettingsDraft | null {
  if (!value) return null

  try {
    const draft = JSON.parse(value) as Partial<SettingsDraft>
    const modeIsValid = draft.pxeMode === 'standalone_dhcp' || draft.pxeMode === 'proxy_dhcp'
    const stringFields: (keyof SettingsDraft)[] = [
      'bindAddress',
      'subnetMask',
      'poolStart',
      'poolEnd',
      'gateway',
      'dnsServers',
      'tftpRoot',
      'biosBootFile',
      'uefiX64BootFile'
    ]

    if (
      !modeIsValid
      || !Number.isFinite(draft.port)
      || !Number.isFinite(draft.leaseSeconds)
      || stringFields.some(field => typeof draft[field] !== 'string')
    ) return null

    return {
      ...draft,
      peName: typeof draft.peName === 'string' ? draft.peName : ''
    } as SettingsDraft
  } catch {
    return null
  }
}

export function pxeSourceDisplayName(source: string): string {
  const name = source.split(/[\\/]/).filter(Boolean).pop() ?? ''
  return name.replace(/\.(?:iso|img)$/i, '')
}

export function controlStartNeedsPe(tftpRoot: string, error?: unknown): boolean {
  if (error === undefined) return false

  const message = String(error).toLowerCase()
  return message.includes('boot package file is missing') && message.includes('boot.wim')
}
