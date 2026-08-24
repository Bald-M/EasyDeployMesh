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

export type PeBrand = 'easyu' | 'edgeless' | 'firpe' | 'hotpe' | 'usm' | 'wepe' | 'unknown'

/**
 * Infers a PE brand from the imported media's display name. Matching is
 * intentionally anchored so custom names that merely contain a brand name do
 * not receive a misleading logo.
 */
export function peBrandFromName(name: string): PeBrand {
  const normalized = name.trim()

  if (/^easyu(?:[_ .-]|$)/i.test(normalized)) return 'easyu'
  if (/^edgeless(?:[_ .-]|$)/i.test(normalized)) return 'edgeless'
  if (/^firpe(?:[_ .-]|$)/i.test(normalized)) return 'firpe'
  if (/^hotpe(?:[_ .-]|$)/i.test(normalized)) return 'hotpe'
  if (/^usm(?:[_ .-]|$)/i.test(normalized)) return 'usm'
  if (/^(?:wepe(?:32|64)?|微pe)(?:[_ .-]|$)/i.test(normalized)) return 'wepe'

  return 'unknown'
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

export function isUnsupportedWepeSource(source: string): boolean {
  return peBrandFromName(pxeSourceDisplayName(source)) === 'wepe'
}

export function controlStartNeedsPe(tftpRoot: string, error?: unknown): boolean {
  if (error === undefined) return false

  const message = String(error).toLowerCase()
  return message.includes('boot package file is missing') && message.includes('boot.wim')
}
