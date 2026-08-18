import { describe, expect, it } from 'vitest'
import { controlStartNeedsPe, parseSettingsDraft, pxeSourceDisplayName } from '../app/utils/settings'

const validDraft = {
  bindAddress: '192.168.1.24',
  port: 7760,
  pxeMode: 'standalone_dhcp',
  subnetMask: '255.255.255.0',
  poolStart: '192.168.1.100',
  poolEnd: '192.168.1.200',
  leaseSeconds: 28800,
  gateway: '',
  dnsServers: '1.1.1.1, 8.8.8.8',
  tftpRoot: '/tmp/tftp',
  peName: 'WePE64_V2.3',
  biosBootFile: 'undionly.kpxe',
  uefiX64BootFile: 'ipxe.efi'
}

describe('settings draft persistence', () => {
  it('restores a complete settings draft', () => {
    expect(parseSettingsDraft(JSON.stringify(validDraft))).toEqual(validDraft)
  })

  it('ignores malformed or outdated drafts', () => {
    expect(parseSettingsDraft('{invalid')).toBeNull()
    expect(parseSettingsDraft(JSON.stringify({ ...validDraft, port: '7760' }))).toBeNull()
    expect(parseSettingsDraft(JSON.stringify({ ...validDraft, biosBootFile: undefined }))).toBeNull()
  })

  it('migrates drafts saved before PE names were recorded', () => {
    const { peName: _, ...oldDraft } = validDraft
    expect(parseSettingsDraft(JSON.stringify(oldDraft))?.peName).toBe('')
  })
})

describe('PXE source display name', () => {
  it.each([
    ['C:\\Images\\WePE64_V2.3.iso', 'WePE64_V2.3'],
    ['/Volumes/PE/FirPE.img', 'FirPE'],
    ['/srv/pxe/Custom WinPE', 'Custom WinPE']
  ])('extracts a readable name from %s', (source, expected) => {
    expect(pxeSourceDisplayName(source)).toBe(expected)
  })
})

describe('control service PE prerequisite', () => {
  it('allows the control service to start before a boot package is imported', () => {
    expect(controlStartNeedsPe('')).toBe(false)
    expect(controlStartNeedsPe('C:\\EasyDeployMesh\\boot')).toBe(false)
  })

  it('recognizes a stale boot package from the backend error', () => {
    expect(controlStartNeedsPe(
      'C:\\EasyDeployMesh\\boot',
      'could not place the Agent bootstrap inside WinPE: boot package file is missing: C:\\EasyDeployMesh\\boot\\boot\\boot.wim'
    )).toBe(true)
    expect(controlStartNeedsPe('C:\\EasyDeployMesh\\boot', 'address already in use')).toBe(false)
  })
})
