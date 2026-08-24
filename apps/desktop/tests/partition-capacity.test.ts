import { describe, expect, it } from 'vitest'
import { partitionCapacity, partitionUserBudgets } from '../app/utils/partition-capacity'
import { windowsAndDataPlan } from '../app/utils/partition-plans'

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
