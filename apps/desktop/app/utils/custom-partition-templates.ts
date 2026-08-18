import type { BootMode, PartitionPlan, PartitionSpec } from '~/types/deployment'
import { partitionPlanFor } from './partition-plans'

export interface CustomPartition {
  role: 'windows' | 'data'
  sizeGib: number | null
  label: string
  driveLetter: string | null
}

export interface CustomPartitionTemplate {
  id: string
  name: string
  partitions: CustomPartition[]
}

export const customPartitionTemplatesStorageKey = 'easydeploymesh.partition-templates.v1'

export function cloneCustomPartitionTemplate(template: CustomPartitionTemplate): CustomPartitionTemplate {
  return {
    id: template.id,
    name: template.name,
    partitions: template.partitions.map(partition => ({ ...partition }))
  }
}

export function customPartitionDisplayName(partition: CustomPartition): string {
  const driveLetter = partition.role === 'windows' ? 'C' : partition.driveLetter || '?'
  return `${driveLetter}: / ${partition.label.trim()}`
}

export function defaultCustomPartitionTemplate(): CustomPartitionTemplate {
  return {
    id: crypto.randomUUID(),
    name: '系统盘 + 软件盘 + 数据盘',
    partitions: [
      { role: 'windows', sizeGib: 30, label: 'Windows', driveLetter: null },
      { role: 'data', sizeGib: 100, label: 'Software', driveLetter: 'D' },
      { role: 'data', sizeGib: null, label: 'Data', driveLetter: 'E' }
    ]
  }
}

export function validateCustomPartitionTemplate(template: CustomPartitionTemplate): string | null {
  if (!template.name.trim()) return 'name'
  if (template.partitions.filter(partition => partition.role === 'windows').length !== 1) return 'windows'
  if (template.partitions.filter(partition => partition.sizeGib === null).length !== 1) return 'remaining'
  const letters = template.partitions.flatMap(partition => partition.driveLetter ? [partition.driveLetter.toUpperCase()] : [])
  if (template.partitions.some(partition => partition.role === 'data' && !partition.driveLetter)) return 'driveLetter'
  if (letters.some(letter => !/^[D-QT-VY-Z]$/.test(letter))) return 'driveLetter'
  if (new Set(letters).size !== letters.length) return 'driveLetter'
  if (template.partitions.some(partition => !/^[A-Za-z0-9 _-]{0,32}$/.test(partition.label))) return 'label'
  if (template.partitions.some(partition => partition.sizeGib !== null && (!Number.isInteger(partition.sizeGib) || partition.sizeGib < 1))) return 'size'
  const windows = template.partitions.find(partition => partition.role === 'windows')!
  return null
}

export function ensureRemainingPartition(template: CustomPartitionTemplate): void {
  if (template.partitions.some(partition => partition.sizeGib === null)) return
  const fallbackPartition = template.partitions.findLast(partition => partition.role === 'data')
    ?? template.partitions.at(-1)
  if (fallbackPartition) fallbackPartition.sizeGib = null
}

export function maximumPartitionSizeGib(
  template: CustomPartitionTemplate,
  partitionIndex: number,
  targetDiskSizesGib: readonly number[]
): number | null {
  if (!targetDiskSizesGib.length) return null
  const smallestTargetSize = Math.min(...targetDiskSizesGib)
  const usedByOtherPartitions = template.partitions.reduce((total, partition, index) =>
    index === partitionIndex ? total : total + (partition.sizeGib ?? 0), 0)
  return Math.max(0, smallestTargetSize - usedByOtherPartitions)
}

export function remainingPartitionSizeGib(
  template: CustomPartitionTemplate,
  partitionIndex: number,
  targetDiskSizesGib: readonly number[]
): number | null {
  return maximumPartitionSizeGib(template, partitionIndex, targetDiskSizesGib)
}

export function setPartitionUsesRemainingSpace(
  template: CustomPartitionTemplate,
  partitionIndex: number,
  usesRemainingSpace: boolean,
  targetDiskSizesGib: readonly number[]
): void {
  const selected = template.partitions[partitionIndex]
  if (!selected) return

  if (!usesRemainingSpace) {
    if (selected.sizeGib === null) {
      selected.sizeGib = Math.max(1, remainingPartitionSizeGib(
        template,
        partitionIndex,
        targetDiskSizesGib
      ) ?? 50)
    }
    return
  }

  template.partitions.forEach((partition, index) => {
    if (index !== partitionIndex && partition.sizeGib === null) {
      partition.sizeGib = Math.max(1, remainingPartitionSizeGib(
        template,
        index,
        targetDiskSizesGib
      ) ?? (partition.role === 'windows' ? 30 : 50))
    }
  })
  selected.sizeGib = null
}

export function clampFixedPartitionSizes(
  template: CustomPartitionTemplate,
  targetDiskSizeGib: number
): void {
  let available = Math.max(0, Math.floor(targetDiskSizeGib))
  for (const partition of template.partitions) {
    if (partition.sizeGib === null) continue
    partition.sizeGib = Math.min(partition.sizeGib, available)
    available -= partition.sizeGib
  }
}

export function customPartitionPlan(template: CustomPartitionTemplate, bootMode: BootMode): PartitionPlan | null {
  if (validateCustomPartitionTemplate(template)) return null
  const base = partitionPlanFor('recommended', bootMode)
  if (!base) return null
  const partitions: PartitionSpec[] = template.partitions.map(partition => ({
    role: partition.role,
    sizeMib: partition.sizeGib === null ? null : partition.sizeGib * 1024,
    fileSystem: 'ntfs',
    label: partition.label.trim(),
    driveLetter: partition.role === 'data' ? partition.driveLetter?.toUpperCase() ?? null : null
  }))
  return { ...base, partitions: [...base.partitions.slice(0, -1), ...partitions] }
}

export function parseCustomPartitionTemplates(value: string | null): CustomPartitionTemplate[] {
  if (!value) return []
  try {
    const templates = JSON.parse(value)
    if (!Array.isArray(templates)) return []
    return templates.filter(template => template && typeof template.id === 'string' && !validateCustomPartitionTemplate(template))
  } catch {
    return []
  }
}
