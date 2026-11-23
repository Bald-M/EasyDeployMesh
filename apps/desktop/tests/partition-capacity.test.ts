import { describe, expect, it } from 'vitest'
import { partitionCapacity, partitionUserBudgets, requiredWindowsPartitionGib, windowsPartitionAvailableBytes, windowsPartitionFitsPayload } from '../app/utils/partition-capacity'
import { legacyBiosMbrPlan, windowsAndDataPlan } from '../app/utils/partition-plans'

describe('partition capacity', () => {
  it.each(['uefi', 'legacy_bios'] as const)('rejects fixed partitions that fill a 100 GiB %s disk', (bootMode) => {
    const plan = windowsAndDataPlan(bootMode, 30)!
    plan.partitions.at(-1)!.sizeMib = 70 * 1024
    plan.partitions.push({
      role: 'data', sizeMib: null, fileSystem: 'ntfs', label: 'Data', driveLetter: 'E'
    })

    const result = partitionCapacity(plan, 100 * 1024 ** 3, 5 * 1024 ** 3)

    expect(result.fits).toBe(false)
    expect(result.requiredMib).toBeGreaterThan(result.availableMib)
    expect(result.remainingMinimumMib).toBe(1024)
  })

  it('derives editor budgets from actual decimal disk bytes and deployment reserves', () => {
    const plan = windowsAndDataPlan('legacy_bios', 30)!
    const budgets = partitionUserBudgets(plan, 100_000_000_000, 10 * 1024 ** 3)

    expect(budgets.fixedGib).toBe(81)
    expect(budgets.totalGib).toBe(82)
  })
})

describe('GHO raw payload capacity', () => {
  const MIB = 1024 * 1024
  const GIB = 1024 * MIB
  const payload = 50 * GIB + 2 * MIB

  it('rounds the required C drive size up to whole GiB', () => {
    expect(requiredWindowsPartitionGib(payload)).toBe(51)
    expect(requiredWindowsPartitionGib(50 * GIB)).toBe(50)
    expect(requiredWindowsPartitionGib(1)).toBe(1)
  })

  it('accepts an exactly equal fixed Windows partition and rejects one byte short', () => {
    const plan = windowsAndDataPlan('legacy_bios', 50)!
    const disk = 120 * GIB
    expect(windowsPartitionFitsPayload(plan, disk, 10 * GIB, 50 * GIB)).toBe(true)
    expect(windowsPartitionFitsPayload(plan, disk, 10 * GIB, 50 * GIB + 1)).toBe(false)
    expect(windowsPartitionFitsPayload(plan, disk, 10 * GIB, payload)).toBe(false)
    expect(windowsPartitionFitsPayload(windowsAndDataPlan('legacy_bios', 51)!, disk, 10 * GIB, payload)).toBe(true)
  })

  it('checks a remaining-space Windows partition against the disk like the backend', () => {
    const plan = legacyBiosMbrPlan()
    const imageBytes = 10 * GIB
    const available = windowsPartitionAvailableBytes(plan, 120 * GIB, imageBytes)
    expect(available).toBe((120 * 1024 - 550 - 10 * 1024 - 512 - 32) * MIB)
    expect(windowsPartitionFitsPayload(plan, 120 * GIB, imageBytes, available)).toBe(true)
    expect(windowsPartitionFitsPayload(plan, 120 * GIB, imageBytes, available + 1)).toBe(false)
  })
})
