import type { BootMode, PartitionPlan } from '~/types/deployment'

export type PartitionPreset = 'recommended' | 'uefi_gpt' | 'legacy_bios_mbr' | 'windows_and_data'

export function uefiGptPlan(): PartitionPlan {
  return {
    table: 'gpt',
    partitions: [
      { role: 'efi', sizeMib: 300, fileSystem: 'fat32', label: 'System' },
      { role: 'msr', sizeMib: 16, fileSystem: null, label: '' },
      { role: 'windows', sizeMib: null, fileSystem: 'ntfs', label: 'Windows' }
    ]
  }
}

export function legacyBiosMbrPlan(): PartitionPlan {
  return {
    table: 'mbr',
    partitions: [
      { role: 'system', sizeMib: 550, fileSystem: 'ntfs', label: 'System Reserved' },
      { role: 'windows', sizeMib: null, fileSystem: 'ntfs', label: 'Windows' }
    ]
  }
}

export function windowsAndDataPlan(bootMode: BootMode, windowsSizeGib: number): PartitionPlan | null {
  const base = partitionPlanFor('recommended', bootMode)
  if (!base || !Number.isInteger(windowsSizeGib) || windowsSizeGib < 1) return null

  return {
    ...base,
    partitions: [
      ...base.partitions.slice(0, -1),
      { role: 'windows', sizeMib: windowsSizeGib * 1024, fileSystem: 'ntfs', label: 'Windows' },
      { role: 'data', sizeMib: null, fileSystem: 'ntfs', label: 'Data' }
    ]
  }
}

export function partitionPlanFor(preset: PartitionPreset, bootMode: BootMode, windowsSizeGib = 30): PartitionPlan | null {
  if (preset === 'windows_and_data') return windowsAndDataPlan(bootMode, windowsSizeGib)
  if (preset === 'uefi_gpt') return uefiGptPlan()
  if (preset === 'legacy_bios_mbr') return legacyBiosMbrPlan()
  if (bootMode === 'uefi') return uefiGptPlan()
  if (bootMode === 'legacy_bios') return legacyBiosMbrPlan()
  return null
}
