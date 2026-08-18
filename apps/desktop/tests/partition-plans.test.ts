import { describe, expect, it } from 'vitest'
import { partitionPlanFor, windowsAndDataPlan } from '../app/utils/partition-plans'

describe('partition plan safety', () => {
  it('does not silently treat an unknown boot mode as legacy BIOS', () => {
    expect(partitionPlanFor('recommended', 'unknown')).toBeNull()
  })

  it('allows an explicit firmware plan when detection is unknown', () => {
    expect(partitionPlanFor('uefi_gpt', 'unknown')?.table).toBe('gpt')
    expect(partitionPlanFor('legacy_bios_mbr', 'unknown')?.table).toBe('mbr')
  })

  it('creates a fixed Windows partition and gives remaining space to data', () => {
    const plan = windowsAndDataPlan('uefi', 30)
    expect(plan?.partitions.at(-2)).toMatchObject({ role: 'windows', sizeMib: 30 * 1024 })
    expect(plan?.partitions.at(-1)).toMatchObject({ role: 'data', sizeMib: null })
  })

  it('allows a small custom system partition so the UI can warn without blocking', () => {
    expect(windowsAndDataPlan('uefi', 10)?.partitions.at(-2)?.sizeMib).toBe(10 * 1024)
  })
})
