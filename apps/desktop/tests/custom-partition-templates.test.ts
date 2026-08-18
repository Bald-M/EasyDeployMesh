import { describe, expect, it, vi } from 'vitest'
import { reactive } from 'vue'
import { clampFixedPartitionSizes, cloneCustomPartitionTemplate, customPartitionDisplayName, customPartitionPlan, defaultCustomPartitionTemplate, ensureRemainingPartition, maximumPartitionSizeGib, parseCustomPartitionTemplates, remainingPartitionSizeGib, setPartitionUsesRemainingSpace } from '../app/utils/custom-partition-templates'

describe('custom partition templates', () => {
  it('builds boot partitions plus C, D and E volumes', () => {
    vi.stubGlobal('crypto', { randomUUID: () => 'template-1' })
    const plan = customPartitionPlan(defaultCustomPartitionTemplate(), 'uefi')
    expect(plan?.partitions.map(partition => [partition.role, partition.sizeMib, partition.driveLetter])).toEqual([
      ['efi', 300, undefined], ['msr', 16, undefined], ['windows', 30720, null],
      ['data', 102400, 'D'], ['data', null, 'E']
    ])
  })

  it('ignores malformed persisted templates', () => {
    expect(parseCustomPartitionTemplates('{bad')).toEqual([])
    expect(parseCustomPartitionTemplates(JSON.stringify([{ id: 'x', name: '', partitions: [] }]))).toEqual([])
  })

  it('allows a Windows partition below the recommendation', () => {
    vi.stubGlobal('crypto', { randomUUID: () => 'template-2' })
    const template = defaultCustomPartitionTemplate()
    template.partitions[0]!.sizeGib = 10
    expect(customPartitionPlan(template, 'uefi')?.partitions.at(2)?.sizeMib).toBe(10 * 1024)
  })

  it('assigns the last data partition when saving a template without remaining space', () => {
    vi.stubGlobal('crypto', { randomUUID: () => 'template-3' })
    const template = defaultCustomPartitionTemplate()
    template.partitions = [
      { role: 'windows', sizeGib: 30, label: 'Windows', driveLetter: null },
      { role: 'data', sizeGib: 20, label: 'Software', driveLetter: 'D' }
    ]

    ensureRemainingPartition(template)

    expect(template.partitions.map(partition => partition.sizeGib)).toEqual([30, null])
    expect(customPartitionPlan(template, 'uefi')).not.toBeNull()
  })

  it('limits a partition to the smallest target disk capacity left by earlier partitions', () => {
    vi.stubGlobal('crypto', { randomUUID: () => 'template-4' })
    const template = defaultCustomPartitionTemplate()

    expect(maximumPartitionSizeGib(template, 1, [60, 80])).toBe(30)
  })

  it('automatically clamps oversized fixed partitions to the target disk', () => {
    vi.stubGlobal('crypto', { randomUUID: () => 'template-5' })
    const template = defaultCustomPartitionTemplate()

    clampFixedPartitionSizes(template, 60)

    expect(template.partitions.map(partition => partition.sizeGib)).toEqual([30, 30, null])
  })

  it('fills the smallest safe remaining capacity and allows the option to be unchecked', () => {
    const template = {
      id: 'template-6',
      name: 'System and data',
      partitions: [
        { role: 'windows' as const, sizeGib: 30, label: 'Windows', driveLetter: null },
        { role: 'data' as const, sizeGib: null, label: 'Data', driveLetter: 'D' }
      ]
    }

    expect(remainingPartitionSizeGib(template, 1, [60, 80])).toBe(30)
    setPartitionUsesRemainingSpace(template, 1, false, [60, 80])
    expect(template.partitions[1]?.sizeGib).toBe(30)
  })

  it('uses the editable volume label in the partition display name', () => {
    expect(customPartitionDisplayName({
      role: 'data',
      sizeGib: 20,
      label: 'Software',
      driveLetter: 'D'
    })).toBe('D: / Software')
  })

  it('creates a plain independent copy from a reactive template before saving', () => {
    vi.stubGlobal('crypto', { randomUUID: () => 'template-7' })
    const draft = reactive(defaultCustomPartitionTemplate())

    const saved = cloneCustomPartitionTemplate(draft)
    draft.name = 'Changed later'
    draft.partitions[0]!.label = 'Changed later'

    expect(saved.name).toBe('系统盘 + 软件盘 + 数据盘')
    expect(saved.partitions[0]?.label).toBe('Windows')
    expect(Object.getPrototypeOf(saved)).toBe(Object.prototype)
  })
})
