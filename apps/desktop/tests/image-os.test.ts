import { describe, expect, it } from 'vitest'
import { detectImageOperatingSystem } from '../app/utils/image-os'

describe('image operating system detection', () => {
  it.each([
    ['WIN7-X64-Intel3X0-AMDRyzen.gho', 'windows-7'],
    ['Windows_10_22H2.wim', 'windows-10'],
    ['win11-pro-x64.esd', 'windows-11'],
    ['Windows 8.1 Enterprise.gho', 'windows-8'],
    ['winxp-sp3.gho', 'windows-xp'],
    ['Windows_Server_2022.wim', 'windows-server'],
    ['win2019-datacenter.wim', 'windows-server']
  ])('recognizes %s as %s', (filename, expected) => {
    expect(detectImageOperatingSystem(filename)).toBe(expected)
  })

  it('uses the source path when the display name has no version', () => {
    expect(detectImageOperatingSystem('system.gho', 'D:\\images\\win10\\system.gho'))
      .toBe('windows-10')
  })

  it('keeps the default icon for unrecognized images', () => {
    expect(detectImageOperatingSystem('backup.gho')).toBe('unknown')
  })

  it('does not mistake unrelated numbers for Windows versions', () => {
    expect(detectImageOperatingSystem('project-10-backup.gho')).toBe('unknown')
  })
})
