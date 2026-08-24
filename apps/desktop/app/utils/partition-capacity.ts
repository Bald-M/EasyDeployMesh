import type { PartitionPlan } from '~/types/deployment'

const MIB_BYTES = 1024 * 1024
const MINIMUM_WINDOWS_MIB = 20 * 1024
const MINIMUM_DATA_MIB = 1024
const IMAGE_CACHE_HEADROOM_MIB = 512
const PARTITION_ALIGNMENT_HEADROOM_MIB = 32

export interface PartitionCapacity {
  fits: boolean
  requiredMib: number
  availableMib: number
  fixedMib: number
  cacheMib: number
  remainingMinimumMib: number
  alignmentHeadroomMib: number
}

export interface PartitionUserBudgets {
  fixedGib: number
  totalGib: number
}

export function partitionCapacity(
  plan: PartitionPlan,
  diskSizeBytes: number,
  imageSizeBytes: number
): PartitionCapacity {
  const availableMib = Math.floor(diskSizeBytes / MIB_BYTES)
  const fixedMib = plan.partitions.reduce((total, partition) => total + (partition.sizeMib ?? 0), 0)
  const cacheMib = Math.ceil(imageSizeBytes / MIB_BYTES) + IMAGE_CACHE_HEADROOM_MIB
  const remaining = plan.partitions.find(partition => partition.sizeMib === null)
  const remainingMinimumMib = remaining?.role === 'data' ? MINIMUM_DATA_MIB : MINIMUM_WINDOWS_MIB
  const requiredMib = fixedMib + cacheMib + remainingMinimumMib + PARTITION_ALIGNMENT_HEADROOM_MIB

  return {
    fits: availableMib >= requiredMib,
    requiredMib,
    availableMib,
    fixedMib,
    cacheMib,
    remainingMinimumMib,
    alignmentHeadroomMib: PARTITION_ALIGNMENT_HEADROOM_MIB
  }
}

export function partitionUserBudgets(
  plan: PartitionPlan,
  diskSizeBytes: number,
  imageSizeBytes: number
): PartitionUserBudgets {
  const capacity = partitionCapacity(plan, diskSizeBytes, imageSizeBytes)
  const bootMib = plan.partitions.reduce((total, partition) =>
    ['efi', 'msr', 'system'].includes(partition.role)
      ? total + (partition.sizeMib ?? 0)
      : total, 0)
  const totalMib = Math.max(0,
    capacity.availableMib
    - bootMib
    - capacity.cacheMib
    - capacity.alignmentHeadroomMib)

  return {
    fixedGib: Math.floor(Math.max(0, totalMib - capacity.remainingMinimumMib) / 1024),
    totalGib: Math.floor(totalMib / 1024)
  }
}
