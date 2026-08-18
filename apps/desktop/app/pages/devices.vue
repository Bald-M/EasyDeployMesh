<script setup lang="ts">
import type { BootMode, ImageArtifact, PartitionPlan, RegisteredDevice } from '~/types/deployment'
import { enqueueDeploymentBatch } from '~/utils/deployment-batch'
import { formatBytes } from '~/utils/files'
import {
  batchDeploymentTargets,
  deploymentLaunchBlocker,
  deployableDeviceIds,
  deviceSelectionState,
  isDeployableDevice,
  reconcileDeviceSelection,
  updateDeviceSelection,
  type DeviceSelectionState
} from '~/utils/device-selection'
import { partitionPlanFor, type PartitionPreset } from '~/utils/partition-plans'
import { templateDiskGroups } from '~/utils/template-disk-groups'
import {
  clampFixedPartitionSizes,
  cloneCustomPartitionTemplate,
  customPartitionDisplayName,
  customPartitionPlan,
  customPartitionTemplatesStorageKey,
  defaultCustomPartitionTemplate,
  ensureRemainingPartition,
  maximumPartitionSizeGib,
  parseCustomPartitionTemplates,
  remainingPartitionSizeGib,
  setPartitionUsesRemainingSpace,
  validateCustomPartitionTemplate,
  type CustomPartitionTemplate
} from '~/utils/custom-partition-templates'

definePageMeta({ titleKey: 'nav.devices' })

type SendMode = 'single' | 'selected' | 'all'

const deviceStore = useDeviceStore()
const runtimeStore = useRuntimeStore()
const imageStore = useImageStore()
const jobStore = useJobStore()
const toast = useToast()
const { locale, t } = useI18n()
let refreshTimer: ReturnType<typeof setInterval> | undefined
const sendDialogOpen = ref(false)
const sendMode = ref<SendMode>('selected')
const sendTarget = ref<RegisteredDevice | null>(null)
const selectedDeviceIds = ref<string[]>([])
const selectedImageId = ref('')
const selectedDiskIds = ref<Record<string, string>>({})
const selectedImageIndex = ref(1)
const partitionPreset = ref<string>('recommended')
const windowsPartitionSizeGib = ref(30)
const customTemplates = ref<CustomPartitionTemplate[]>([])
const templateDialogOpen = ref(false)
const templateDraft = ref<CustomPartitionTemplate | null>(null)
const deploymentConfirmed = ref(false)
const confirmedDeploymentKey = ref<string | null>(null)
const sending = ref(false)
const sendProgress = ref({ completed: 0, total: 0 })
const hardwareTarget = ref<RegisteredDevice | null>(null)
const hardwareDialogOpen = ref(false)

const verifiedImages = computed(() => imageStore.images.filter(image => image.verified && ['gho', 'wim', 'esd'].includes(image.format)))
const imageItems = computed(() => verifiedImages.value.map(image => ({
  label: `${image.name} · ${image.format.toUpperCase()} · ${formatBytes(image.sizeBytes, locale.value)}`,
  value: image.id
})))
const eligibleDevices = computed(() => deviceStore.devices.filter(isDeployableDevice))
const selectedDeviceIdSet = computed(() => new Set(selectedDeviceIds.value))
const selectedDevices = computed(() => batchDeploymentTargets('selected', selectedDeviceIds.value, deviceStore.devices))
const selectedDeviceCount = computed(() => selectedDevices.value.length)
const canSendSelection = computed(() => selectedDeviceCount.value > 0 && verifiedImages.value.length > 0)
const selectAllState = computed<DeviceSelectionState>({
  get: () => deviceSelectionState(selectedDeviceIds.value, deviceStore.devices),
  set: (value) => {
    selectedDeviceIds.value = value === true
      ? deployableDeviceIds(deviceStore.devices)
      : []
  }
})
const sendSelection = computed(() => sendMode.value === 'selected')
const sendAll = computed(() => sendMode.value === 'all')
const sendBatch = computed(() => sendMode.value !== 'single')
const currentSendTarget = computed(() => {
  const targetId = sendTarget.value?.device.id
  return targetId
    ? deviceStore.devices.find(entry => entry.device.id === targetId) ?? null
    : null
})
const selectedImage = computed(() => verifiedImages.value.find(image => image.id === selectedImageId.value) ?? null)
const sendTargets = computed(() => {
  const mode = sendMode.value
  if (mode === 'single') {
    return currentSendTarget.value ? [currentSendTarget.value] : []
  }
  return batchDeploymentTargets(mode, selectedDeviceIds.value, deviceStore.devices)
})
const sendTargetCount = computed(() => sendTargets.value.length)
const templateTargetDiskGroups = computed(() => templateDiskGroups(sendTargets.value, selectedDiskIds.value))
const templateSelectedDisks = computed(() => templateTargetDiskGroups.value.flatMap(group =>
  group.disks.filter(row => row.selected)
))
const templateTargetDiskSizesGib = computed(() => templateSelectedDisks.value.map(({ disk }) =>
  Math.floor(disk.sizeBytes / (1024 ** 3))
))
const smallestTemplateTargetDiskGib = computed(() => templateTargetDiskSizesGib.value.length
  ? Math.min(...templateTargetDiskSizesGib.value)
  : null
)
const sendTargetsAreDeployable = computed(() => sendTargets.value.every(isDeployableDevice))
const sendDialogTitle = computed(() => {
  if (sendAll.value) return t('devices.sendAllTitle', { count: sendTargetCount.value })
  if (sendSelection.value) return t('devices.sendSelectedTitle', { count: sendTargetCount.value })
  return t('devices.sendTitle')
})
const sendDialogDescription = computed(() => {
  if (sendAll.value) return t('devices.sendAllDescription', { count: sendTargetCount.value })
  if (sendSelection.value) return t('devices.sendSelectedDescription', { count: sendTargetCount.value })
  return t('devices.sendDescription')
})
const batchTargetsTitle = computed(() => t(
  sendAll.value ? 'devices.sendAllTargets' : 'devices.sendSelectedTargets',
  { count: sendTargetCount.value }
))
const batchDiskHint = computed(() => t(
  sendAll.value ? 'devices.sendAllDiskHint' : 'devices.sendSelectedDiskHint'
))
const batchTargetCountLabel = computed(() => t(
  sendAll.value ? 'devices.allTargetCount' : 'devices.selectedCount',
  { count: sendTargetCount.value }
))
const sendActionLabel = computed(() => {
  if (sending.value) return t('devices.sendingProgressShort', sendProgress.value)
  if (sendAll.value) return t('devices.sendAllCount', { count: sendTargetCount.value })
  if (sendSelection.value) return t('devices.sendSelectedCount', { count: sendTargetCount.value })
  return t('devices.send')
})
function selectedPartitionPlan(bootMode: BootMode) {
  if (partitionPreset.value.startsWith('custom:')) {
    const template = customTemplates.value.find(item => `custom:${item.id}` === partitionPreset.value)
    return template ? customPartitionPlan(template, bootMode) : null
  }
  return partitionPlanFor(partitionPreset.value as PartitionPreset, bootMode, windowsPartitionSizeGib.value)
}

const selectedPlansAreKnown = computed(() => sendTargets.value.every(entry => selectedPartitionPlan(entry.device.bootMode) !== null))
const partitionPreviewGroups = computed(() => {
  const groups = new Map<string, {
    plan: PartitionPlan
    count: number
    bootModes: Set<BootMode>
  }>()

  for (const entry of sendTargets.value) {
    const plan = selectedPartitionPlan(entry.device.bootMode)
    if (!plan) continue
    const key = JSON.stringify(plan)
    const existing = groups.get(key)
    if (existing) {
      existing.count += 1
      existing.bootModes.add(entry.device.bootMode)
    } else {
      groups.set(key, {
        plan,
        count: 1,
        bootModes: new Set([entry.device.bootMode])
      })
    }
  }

  return [...groups.entries()].map(([key, group]) => ({
    key,
    plan: group.plan,
    count: group.count,
    bootModes: [...group.bootModes]
  }))
})
const targetsWithoutPartitionPlan = computed(() => sendTargets.value.filter(entry =>
  selectedPartitionPlan(entry.device.bootMode) === null
))
const partitionPlanErrorDescription = computed(() => {
  if (targetsWithoutPartitionPlan.value.length <= 1) {
    return t('devices.bootModeRequiredHint')
  }
  return t('devices.bootModeRequiredSelectedHint', {
    names: new Intl.ListFormat(locale.value, {
      style: 'short',
      type: 'conjunction'
    }).format(targetsWithoutPartitionPlan.value.map(displayName))
  })
})
const partitionPresetItems = computed(() => [
  { label: t('devices.partitionRecommended'), value: 'recommended' },
  { label: t('devices.partitionWindowsAndData'), value: 'windows_and_data' },
  { label: t('devices.partitionUefi'), value: 'uefi_gpt' },
  { label: t('devices.partitionLegacy'), value: 'legacy_bios_mbr' },
  ...customTemplates.value.map(template => ({ label: `${t('devices.customTemplatePrefix')} · ${template.name}`, value: `custom:${template.id}` }))
])
const sendTargetDisksAreValid = computed(() => sendTargets.value.every(entry =>
  selectedDisk(entry) !== null
))
const deploymentConfirmationKey = computed(() => JSON.stringify({
  mode: sendMode.value,
  image: selectedImage.value
    ? {
        id: selectedImage.value.id,
        sha256: selectedImage.value.sha256,
        sizeBytes: selectedImage.value.sizeBytes,
        verified: selectedImage.value.verified
      }
    : null,
  imageIndex: selectedImageIndex.value,
  partitionPreset: partitionPreset.value,
  windowsPartitionSizeGib: partitionPreset.value === 'windows_and_data' ? windowsPartitionSizeGib.value : null,
  targets: sendTargets.value
    .map((entry) => {
      const disk = selectedDisk(entry)
      return {
        deviceId: entry.device.id,
        online: entry.online,
        bootMode: entry.device.bootMode,
        partitionPlan: selectedPartitionPlan(entry.device.bootMode),
        disk: disk
          ? {
              id: disk.id,
              model: disk.model,
              serial: disk.serial,
              sizeBytes: disk.sizeBytes
            }
          : null
      }
    })
    .sort((left, right) => left.deviceId.localeCompare(right.deviceId))
}))
const deploymentConfirmation = computed({
  get: () => deploymentConfirmed.value,
  set: (confirmed: boolean) => {
    deploymentConfirmed.value = confirmed
    confirmedDeploymentKey.value = confirmed
      ? deploymentConfirmationKey.value
      : null
  }
})
const canSend = computed(() => Boolean(
  selectedImage.value
  && deploymentConfirmed.value
  && confirmedDeploymentKey.value === deploymentConfirmationKey.value
  && sendTargetCount.value > 0
  && sendTargetsAreDeployable.value
  && selectedPlansAreKnown.value
  && sendTargetDisksAreValid.value
  && !sending.value
))

function operationFor(image: ImageArtifact) {
  return image.format === 'gho' ? 'deploy_gho' as const : 'deploy_wim' as const
}

function smallWindowsPartitionGib(plan: PartitionPlan | null) {
  const sizeMib = plan?.partitions.find(partition => partition.role === 'windows')?.sizeMib
  return sizeMib !== null && sizeMib !== undefined && sizeMib < 20 * 1024
    ? sizeMib / 1024
    : null
}

function defaultDisk(entry: RegisteredDevice) {
  return entry.device.disks.find(disk => !disk.isSystem) ?? entry.device.disks[0]
}

function selectedDisk(entry: RegisteredDevice) {
  const diskId = selectedDiskIds.value[entry.device.id]
  return entry.device.disks.find(disk => disk.id === diskId) ?? null
}

function initializeTargetDisks(entries: readonly RegisteredDevice[]) {
  selectedDiskIds.value = Object.fromEntries(entries.flatMap((entry) => {
    const disk = defaultDisk(entry)
    return disk ? [[entry.device.id, disk.id]] : []
  }))
}

function targetDiskItems(entry: RegisteredDevice) {
  return entry.device.disks.map(disk => ({
    label: `${disk.model || t('devices.unknownDisk')} · ${formatBytes(disk.sizeBytes, locale.value)}${disk.isSystem ? ` · ${t('devices.systemDisk')}` : ''}`,
    value: disk.id
  }))
}

function updateTargetDisk(entry: RegisteredDevice, value: string) {
  selectedDiskIds.value = {
    ...selectedDiskIds.value,
    [entry.device.id]: value
  }
}

function openSendDialog(mode: SendMode, entry: RegisteredDevice | null = null) {
  const targets = mode === 'single'
    ? (entry && isDeployableDevice(entry) ? [entry] : [])
    : batchDeploymentTargets(mode, selectedDeviceIds.value, deviceStore.devices)
  const blocker = deploymentLaunchBlocker(targets.length, verifiedImages.value.length)
  if (blocker === 'targets') return
  if (blocker === 'images') {
    toast.add({
      title: t('devices.sendNoImageTitle'),
      description: t('devices.sendNoImageHint'),
      color: 'warning',
      icon: 'i-lucide-image-off'
    })
    return
  }
  sendMode.value = mode
  sendTarget.value = mode === 'single' ? entry : null
  selectedImageId.value = verifiedImages.value[0]?.id ?? ''
  initializeTargetDisks(targets)
  selectedImageIndex.value = 1
  partitionPreset.value = 'recommended'
  windowsPartitionSizeGib.value = 30
  deploymentConfirmation.value = false
  sendProgress.value = { completed: 0, total: targets.length }
  sendDialogOpen.value = true
}

function isDeviceSelected(entry: RegisteredDevice) {
  return selectedDeviceIdSet.value.has(entry.device.id)
}

function setDeviceSelected(entry: RegisteredDevice, value: boolean | 'indeterminate') {
  if (!isDeployableDevice(entry)) return
  selectedDeviceIds.value = updateDeviceSelection(
    selectedDeviceIds.value,
    entry.device.id,
    value === true
  )
}

function clearDeviceSelection() {
  selectedDeviceIds.value = []
}

function selectionHint(entry: RegisteredDevice) {
  if (!entry.online) return t('devices.selectOfflineHint')
  if (!entry.device.disks.length) return t('devices.selectNoDiskHint')
  return t('devices.selectDevice', { name: displayName(entry) })
}

function bootModeSummary(bootModes: readonly BootMode[]) {
  return bootModes.map(mode => t(`devices.bootModes.${mode}`)).join(' / ')
}

function openHardwareDialog(entry: RegisteredDevice) {
  hardwareTarget.value = entry
  hardwareDialogOpen.value = true
}

function processorSummary(entry: RegisteredDevice) {
  const physical = entry.device.physicalCoreCount
  const logical = entry.device.logicalProcessorCount
  if (!physical && !logical) return t('devices.unknownHardware')
  if (physical && logical) return t('devices.cpuCounts', { cores: physical, threads: logical })
  return t('devices.cpuThreads', { threads: logical })
}

function diskModelSummary(entry: RegisteredDevice) {
  const models = entry.device.disks
    .map(disk => disk.model.trim())
    .filter(Boolean)

  if (!models.length) return t('devices.unknownDisk')
  if (models.length === 1) return models[0]
  return t('devices.diskModelSummary', { model: models[0], count: models.length - 1 })
}

function formatUptime(seconds: number | null) {
  if (seconds === null) return t('devices.unknownHardware')
  const days = Math.floor(seconds / 86400)
  const hours = Math.floor((seconds % 86400) / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  return t('devices.uptimeValue', { days, hours, minutes })
}

async function handleSend() {
  const image = selectedImage.value
  if (!image || !canSend.value) return
  const smallSize = sendTargets.value
    .map(entry => smallWindowsPartitionGib(selectedPartitionPlan(entry.device.bootMode)))
    .find(size => size !== null)
  if (smallSize !== undefined && smallSize !== null
    && !window.confirm(t('devices.smallWindowsPartitionWarning', { size: smallSize }))) return
  const recipients = [...sendTargets.value]
  const confirmedKey = confirmedDeploymentKey.value
  if (!confirmedKey || confirmedKey !== deploymentConfirmationKey.value) return
  const imageIndex = selectedImageIndex.value

  const deployments = recipients.map((entry) => {
    const disk = selectedDisk(entry)
    const partitionPlan = selectedPartitionPlan(entry.device.bootMode)
    return disk && partitionPlan ? { entry, disk, partitionPlan } : null
  })
  if (deployments.some(deployment => deployment === null)) return

  const activeSendMode = sendMode.value
  const requests = deployments.map((deployment) => {
    const { entry, disk, partitionPlan } = deployment!
    return {
      name: t('devices.sendJobName', { image: image.name, device: displayName(entry) }),
      operation: operationFor(image),
      imageId: image.id,
      target: {
        deviceId: entry.device.id,
        targetDiskId: disk.id,
        targetDiskModel: disk.model,
        targetDiskSerial: disk.serial,
        targetDiskSizeBytes: disk.sizeBytes
      },
      options: {
        imageIndex,
        partitionPlan
      }
    }
  })
  sendProgress.value = { completed: 0, total: requests.length }
  sending.value = true

  try {
    const result = await enqueueDeploymentBatch(requests, {
      confirmationIsValid: () => deploymentConfirmed.value
        && confirmedDeploymentKey.value === confirmedKey
        && deploymentConfirmationKey.value === confirmedKey,
      enqueue: request => jobStore.enqueue(request),
      onProgress: progress => {
        sendProgress.value = progress
      }
    })
    const queuedCount = result.queuedDeviceIds.length

    if (result.interrupted) {
      const notQueuedDeviceIds = [...result.failedDeviceIds, ...result.pendingDeviceIds]
      const retryableDeviceIds = reconcileDeviceSelection(notQueuedDeviceIds, deviceStore.devices)
      if (activeSendMode !== 'single') {
        selectedDeviceIds.value = retryableDeviceIds
        sendMode.value = 'selected'
        sendTarget.value = null
        initializeTargetDisks(batchDeploymentTargets(
          'selected',
          retryableDeviceIds,
          deviceStore.devices
        ))
      }
      deploymentConfirmation.value = false
      toast.add({
        title: t('devices.sendInterrupted'),
        description: t('devices.sendInterruptedHint', {
          success: queuedCount,
          remaining: notQueuedDeviceIds.length,
          retryable: retryableDeviceIds.length
        }),
        color: 'warning',
        icon: 'i-lucide-triangle-alert'
      })
      return
    }

    if (queuedCount === recipients.length) {
      sendDialogOpen.value = false
      if (activeSendMode === 'selected') clearDeviceSelection()
      toast.add({
        title: t('devices.sendQueued'),
        description: t('devices.sendQueuedHint', { count: queuedCount }),
        color: 'success',
        icon: 'i-lucide-send'
      })
      return
    }

    sendDialogOpen.value = false
    const retryableFailedDeviceIds = reconcileDeviceSelection(result.failedDeviceIds, deviceStore.devices)
    if (activeSendMode !== 'single') {
      selectedDeviceIds.value = retryableFailedDeviceIds
    }
    toast.add({
      title: queuedCount > 0 ? t('devices.sendPartiallyQueued') : t('devices.sendFailed'),
      description: queuedCount > 0
        ? t(activeSendMode === 'all'
          ? 'devices.sendAllPartiallyQueuedHint'
          : 'devices.sendPartiallyQueuedHint', {
            success: queuedCount,
            failed: result.failedDeviceIds.length,
            retryable: retryableFailedDeviceIds.length
          })
        : jobStore.lastError ?? undefined,
      color: queuedCount > 0 ? 'warning' : 'error',
      icon: queuedCount > 0 ? 'i-lucide-triangle-alert' : 'i-lucide-circle-alert'
    })
  } finally {
    sending.value = false
  }
}

function displayName(entry: RegisteredDevice) {
  return entry.device.hostname || entry.device.macAddress
}

function diskCapacity(entry: RegisteredDevice) {
  return entry.device.disks.reduce((total, disk) => total + disk.sizeBytes, 0)
}

function formatLastSeen(value: string) {
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'medium'
  }).format(new Date(value))
}

async function handleRemove(entry: RegisteredDevice) {
  if (!window.confirm(t('devices.removeConfirm', {
    name: displayName(entry)
  }))) {
    return
  }

  try {
    await deviceStore.remove(entry.device.id)
    toast.add({
      title: t('devices.removed'),
      color: 'success',
      icon: 'i-lucide-monitor-x'
    })
  } catch {
    toast.add({
      title: t('devices.removeFailed'),
      description: deviceStore.lastError ?? undefined,
      color: 'error',
      icon: 'i-lucide-circle-alert'
    })
  }
}

function newTemplate() {
  templateDraft.value = defaultCustomPartitionTemplate()
  clampDraftPartitionSizes()
  templateDialogOpen.value = true
}

function editSelectedTemplate() {
  const template = customTemplates.value.find(item => `custom:${item.id}` === partitionPreset.value)
  templateDraft.value = cloneCustomPartitionTemplate(template ?? defaultCustomPartitionTemplate())
  clampDraftPartitionSizes()
  templateDialogOpen.value = true
}

function clampDraftPartitionSizes() {
  const draft = templateDraft.value
  const targetSize = smallestTemplateTargetDiskGib.value
  if (draft && targetSize !== null) clampFixedPartitionSizes(draft, targetSize)
}

function maximumDraftPartitionSizeGib(index: number) {
  const draft = templateDraft.value
  if (!draft) return undefined
  return maximumPartitionSizeGib(draft, index, templateTargetDiskSizesGib.value) ?? undefined
}

function addDataPartition() {
  if (!templateDraft.value) return
  const used = new Set(templateDraft.value.partitions.flatMap(partition => partition.driveLetter ? [partition.driveLetter] : []))
  const driveLetter = ['D', 'E', 'F', 'G', 'H', 'I', 'J'].find(letter => !used.has(letter)) ?? 'D'
  const remainingIndex = templateDraft.value.partitions.findIndex(partition => partition.sizeGib === null)
  templateDraft.value.partitions.splice(Math.max(remainingIndex, 1), 0, {
    role: 'data', sizeGib: 50, label: `Data ${driveLetter}`, driveLetter
  })
  clampDraftPartitionSizes()
}

function removeDraftPartition(index: number) {
  if (!templateDraft.value || templateDraft.value.partitions[index]?.role === 'windows') return
  templateDraft.value.partitions.splice(index, 1)
}

function setRemainingPartition(index: number, remaining: boolean) {
  if (!templateDraft.value) return
  setPartitionUsesRemainingSpace(
    templateDraft.value,
    index,
    remaining,
    templateTargetDiskSizesGib.value
  )
  clampDraftPartitionSizes()
}

function remainingSizeGib(index: number) {
  if (templateDraft.value?.partitions[index]?.sizeGib !== null) return null
  return remainingPartitionSizeGib(
    templateDraft.value,
    index,
    templateTargetDiskSizesGib.value
  )
}

function saveTemplate() {
  const draft = templateDraft.value
  if (!draft) return
  clampDraftPartitionSizes()
  ensureRemainingPartition(draft)
  const error = validateCustomPartitionTemplate(draft)
  if (error) {
    toast.add({ title: t('devices.templateInvalid'), description: t(`devices.templateErrors.${error}`), color: 'error' })
    return
  }
  const windowsSize = draft.partitions.find(partition => partition.role === 'windows')?.sizeGib
  if (windowsSize !== null && windowsSize !== undefined && windowsSize < 20
    && !window.confirm(t('devices.smallWindowsPartitionWarning', { size: windowsSize }))) return
  const index = customTemplates.value.findIndex(template => template.id === draft.id)
  const savedTemplate = cloneCustomPartitionTemplate(draft)
  const nextTemplates = index === -1
    ? [...customTemplates.value, savedTemplate]
    : customTemplates.value.map((template, templateIndex) => templateIndex === index ? savedTemplate : template)
  try {
    localStorage.setItem(customPartitionTemplatesStorageKey, JSON.stringify(nextTemplates))
    customTemplates.value = nextTemplates
    partitionPreset.value = `custom:${draft.id}`
    templateDialogOpen.value = false
    toast.add({ title: t('devices.templateSaved'), color: 'success', icon: 'i-lucide-check' })
  } catch {
    toast.add({ title: t('devices.templateSaveFailed'), color: 'error', icon: 'i-lucide-circle-alert' })
  }
}

function deleteTemplate() {
  const id = templateDraft.value?.id
  if (!id) return
  customTemplates.value = customTemplates.value.filter(template => template.id !== id)
  localStorage.setItem(customPartitionTemplatesStorageKey, JSON.stringify(customTemplates.value))
  if (partitionPreset.value === `custom:${id}`) partitionPreset.value = 'recommended'
  templateDialogOpen.value = false
}

watch(
  () => deviceStore.devices,
  (entries) => {
    const reconciled = reconcileDeviceSelection(selectedDeviceIds.value, entries)
    if (reconciled.length !== selectedDeviceIds.value.length
      || reconciled.some((id, index) => id !== selectedDeviceIds.value[index])) {
      selectedDeviceIds.value = reconciled
    }
  }
)

watch(sendTargets, (targets) => {
  if (!sendDialogOpen.value || !sendBatch.value) return

  const next = { ...selectedDiskIds.value }
  let changed = false
  for (const entry of targets) {
    if (entry.device.disks.some(disk => disk.id === next[entry.device.id])) continue
    const disk = defaultDisk(entry)
    if (disk) {
      next[entry.device.id] = disk.id
      changed = true
    }
  }
  if (changed) selectedDiskIds.value = next
})

watch(deploymentConfirmationKey, (current, previous) => {
  if (current !== previous && deploymentConfirmed.value) {
    deploymentConfirmation.value = false
  }
})

watch(smallestTemplateTargetDiskGib, () => {
  if (templateDialogOpen.value) clampDraftPartitionSizes()
})

onMounted(() => {
  customTemplates.value = parseCustomPartitionTemplates(localStorage.getItem(customPartitionTemplatesStorageKey))
  deviceStore.refresh()
  refreshTimer = setInterval(() => deviceStore.refresh(), 5_000)
})

onBeforeUnmount(() => {
  if (refreshTimer) {
    clearInterval(refreshTimer)
  }
})
</script>

<template>
  <div class="mx-auto max-w-[1500px] p-6 lg:p-8">
    <PageHeader
      :title="$t('devices.title')"
      :description="$t('devices.description')"
    >
      <template #actions>
        <UButton
          icon="i-lucide-send"
          :label="selectedDeviceCount
            ? $t('devices.sendSelectedCount', { count: selectedDeviceCount })
            : $t('devices.sendSelected')"
          :color="canSendSelection ? 'primary' : 'neutral'"
          :disabled="selectedDeviceCount === 0"
          :title="!verifiedImages.length ? $t('devices.sendNoImageHint') : undefined"
          @click="openSendDialog('selected')"
        />
        <UTooltip :text="$t('devices.sendAllHint', { count: eligibleDevices.length })">
          <UButton
            icon="i-lucide-send-to-back"
            color="neutral"
            variant="outline"
            :label="$t('devices.sendAll')"
            :aria-label="$t('devices.sendAllCount', { count: eligibleDevices.length })"
            :disabled="eligibleDevices.length === 0"
            @click="openSendDialog('all')"
          />
        </UTooltip>
        <UButton
          icon="i-lucide-refresh-cw"
          color="neutral"
          variant="outline"
          :label="$t('common.refresh')"
          :loading="deviceStore.loading"
          @click="deviceStore.verifyOnline"
        />
      </template>
    </PageHeader>

    <div v-if="deviceStore.devices.length || deviceStore.pendingPxeClients.length" class="mt-6 space-y-4">
      <UCard v-if="deviceStore.pendingPxeClients.length">
        <template #header>
          <div><h2 class="text-base font-semibold">{{ $t('devices.pxeDiscovered') }}</h2><p class="mt-1 text-sm text-muted">{{ $t('devices.pxeDiscoveredHint') }}</p></div>
        </template>
        <div class="-mx-6 -my-5 divide-y divide-default">
          <article v-for="client in deviceStore.pendingPxeClients" :key="client.macAddress" class="grid grid-cols-4 items-center gap-5 px-6 py-4">
            <div><p class="font-mono text-sm font-semibold">{{ client.macAddress }}</p><p class="mt-1 text-xs text-dimmed">{{ client.ipAddress || '—' }}</p></div>
            <UBadge color="warning" variant="subtle">{{ $t(`devices.pxeStages.${client.stage}`) }}</UBadge>
            <p class="text-sm">{{ $t(`devices.architectures.${client.architecture}`) }}</p>
            <p class="text-right text-xs text-muted">{{ formatLastSeen(client.lastSeenAt) }}</p>
          </article>
        </div>
      </UCard>
      <div v-if="deviceStore.devices.length" class="grid grid-cols-3 gap-4">
        <UCard>
          <div class="flex items-center gap-4">
            <div class="grid size-11 place-items-center rounded-xl bg-success/10 text-success">
              <UIcon name="i-lucide-monitor-check" class="size-5" />
            </div>
            <div>
              <p class="text-2xl font-semibold">
                {{ deviceStore.onlineDevices.length }}
              </p>
              <p class="text-xs text-muted">
                {{ $t('devices.onlineNow') }}
              </p>
            </div>
          </div>
        </UCard>
        <UCard>
          <div class="flex items-center gap-4">
            <div class="grid size-11 place-items-center rounded-xl bg-primary/10 text-primary">
              <UIcon name="i-lucide-monitor-smartphone" class="size-5" />
            </div>
            <div>
              <p class="text-2xl font-semibold">
                {{ deviceStore.devices.length }}
              </p>
              <p class="text-xs text-muted">
                {{ $t('devices.registeredTotal') }}
              </p>
            </div>
          </div>
        </UCard>
        <UCard>
          <div class="flex items-center gap-4">
            <div class="grid size-11 place-items-center rounded-xl bg-warning/10 text-warning">
              <UIcon name="i-lucide-hard-drive" class="size-5" />
            </div>
            <div>
              <p class="text-2xl font-semibold">
                {{ deviceStore.devices.reduce((total, entry) => total + entry.device.disks.length, 0) }}
              </p>
              <p class="text-xs text-muted">
                {{ $t('devices.reportedDisks') }}
              </p>
            </div>
          </div>
        </UCard>
      </div>

      <UCard v-if="deviceStore.devices.length" class="overflow-hidden">
        <template #header>
          <div class="flex items-center justify-between gap-4">
            <div>
              <h2 class="text-base font-semibold">
                {{ $t('devices.registered') }}
              </h2>
              <p class="mt-1 text-sm text-muted">
                {{ $t('devices.registeredHint') }}
              </p>
            </div>
            <div class="flex flex-wrap items-center justify-end gap-2.5">
              <UCheckbox
                v-model="selectAllState"
                :label="$t('devices.selectAllDeployable')"
                :disabled="!eligibleDevices.length"
                size="sm"
              />
              <UBadge color="primary" variant="subtle">
                {{ $t('devices.selectedOfDeployable', {
                  selected: selectedDeviceCount,
                  total: eligibleDevices.length
                }) }}
              </UBadge>
              <UButton
                v-if="selectedDeviceCount"
                color="neutral"
                variant="ghost"
                size="xs"
                :label="$t('devices.clearSelection')"
                @click="clearDeviceSelection"
              />
              <UBadge
                :color="runtimeStore.controlStatus.state === 'running' ? 'success' : 'neutral'"
                variant="subtle"
              >
                {{ $t(`common.${runtimeStore.controlStatus.state}`) }}
              </UBadge>
            </div>
          </div>
        </template>

        <div class="-mx-6 -my-5 divide-y divide-default">
          <article
            v-for="entry in deviceStore.devices"
            :key="entry.device.id"
            class="grid grid-cols-[20px_minmax(190px,1.2fr)_130px_minmax(210px,1.1fr)_minmax(190px,1fr)_104px] items-center gap-3 px-6 py-4 transition-colors"
            :class="isDeviceSelected(entry) ? 'bg-primary/5 hover:bg-primary/10' : 'hover:bg-elevated/45'"
          >
            <div class="flex items-center" :title="selectionHint(entry)">
              <UCheckbox
                :model-value="isDeviceSelected(entry)"
                :disabled="!isDeployableDevice(entry)"
                :aria-label="$t('devices.selectDevice', { name: displayName(entry) })"
                @update:model-value="setDeviceSelected(entry, $event)"
              />
            </div>

            <div class="flex min-w-0 items-center gap-3">
              <div
                class="relative grid size-11 shrink-0 place-items-center rounded-xl ring-1"
                :class="entry.online
                  ? 'bg-success/10 text-success ring-success/20'
                  : 'bg-elevated text-muted ring-default'"
              >
                <UIcon name="i-lucide-monitor" class="size-5" />
                <span
                  class="absolute -bottom-0.5 -right-0.5 size-3 rounded-full border-2 border-default"
                  :class="entry.online ? 'bg-success' : 'bg-neutral-400'"
                />
              </div>
              <div class="min-w-0">
                <div class="flex items-center gap-2">
                  <p class="truncate text-sm font-semibold">
                    {{ displayName(entry) }}
                  </p>
                  <UBadge
                    :color="entry.online ? 'success' : 'neutral'"
                    variant="subtle"
                    size="sm"
                  >
                    {{ $t(entry.online ? 'common.online' : 'common.offline') }}
                  </UBadge>
                </div>
                <p class="mt-1 truncate font-mono text-[11px] text-dimmed">
                  {{ entry.device.macAddress }}
                </p>
              </div>
            </div>

            <div>
              <p class="text-[11px] uppercase tracking-wide text-dimmed">
                {{ $t('devices.network') }}
              </p>
              <p class="mt-1.5 font-mono text-xs">
                {{ entry.device.ipAddress }}
              </p>
              <p class="mt-1 text-[11px] text-dimmed">
                Agent v{{ entry.agentVersion }}
              </p>
            </div>

            <div>
              <p class="text-[11px] uppercase tracking-wide text-dimmed">
                {{ $t('devices.hardwareOverview') }}
              </p>
              <div class="mt-1.5 space-y-1.5">
                <div class="flex min-w-0 items-center gap-1.5">
                  <UIcon name="i-lucide-cpu" class="size-3.5 shrink-0 text-primary" />
                  <span
                    class="truncate text-xs font-medium"
                    :title="entry.device.cpuModel || $t('devices.unknownHardware')"
                  >
                    {{ entry.device.cpuModel || $t('devices.unknownHardware') }}
                  </span>
                </div>
                <div class="flex min-w-0 items-center gap-1.5">
                  <UIcon name="i-lucide-memory-stick" class="size-3.5 shrink-0 text-primary" />
                  <span class="truncate text-xs font-medium">
                    {{ entry.device.memoryBytes ? formatBytes(entry.device.memoryBytes, locale) : $t('devices.unknownHardware') }} RAM
                  </span>
                </div>
              </div>
              <p class="mt-1.5 truncate text-[10px] text-dimmed">
                {{ processorSummary(entry) }} · {{ $t(`devices.architectures.${entry.device.architecture}`) }} · {{ $t(`devices.bootModes.${entry.device.bootMode}`) }}
              </p>
            </div>

            <div>
              <p class="text-[11px] uppercase tracking-wide text-dimmed">
                {{ $t('devices.storage') }}
              </p>
              <div class="mt-1.5 flex min-w-0 items-center gap-1.5">
                <UIcon name="i-lucide-hard-drive" class="size-4 shrink-0 text-primary" />
                <p
                  class="truncate text-sm font-medium"
                  :title="entry.device.disks.map(disk => disk.model || $t('devices.unknownDisk')).join(' / ')"
                >
                  {{ diskModelSummary(entry) }}
                </p>
              </div>
              <p class="mt-1 text-[11px] text-dimmed">
                {{ $t('devices.diskCount', { count: entry.device.disks.length }) }}
                · {{ formatBytes(diskCapacity(entry), locale) }}
              </p>
              <p
                class="mt-1 truncate text-[11px] text-dimmed"
                :title="formatLastSeen(entry.device.lastSeenAt)"
              >
                {{ $t('devices.lastSeen') }} {{ formatLastSeen(entry.device.lastSeenAt) }}
              </p>
            </div>

            <div class="flex justify-end gap-1">
              <UButton icon="i-lucide-info" color="neutral" variant="ghost" size="sm" :aria-label="$t('devices.hardwareDetails')" @click="openHardwareDialog(entry)" />
              <UTooltip :text="!entry.online ? $t('devices.sendOfflineHint') : !entry.device.disks.length ? $t('devices.sendNoDiskHint') : !verifiedImages.length ? $t('devices.sendNoImageHint') : $t('devices.send')">
                <UButton icon="i-lucide-send" variant="soft" size="sm" :disabled="!entry.online || !entry.device.disks.length" :aria-label="$t('devices.send')" @click="openSendDialog('single', entry)" />
              </UTooltip>
              <UButton icon="i-lucide-trash-2" color="error" variant="ghost" size="sm" :aria-label="$t('common.delete')" @click="handleRemove(entry)" />
            </div>
          </article>
        </div>
      </UCard>
    </div>

    <EmptyStatePanel
      v-else-if="!deviceStore.loading"
      icon="i-lucide-monitor-smartphone"
      :title="$t('devices.emptyTitle')"
      :description="runtimeStore.controlStatus.state === 'running'
        ? $t('devices.waitingForAgents')
        : $t('devices.emptyDescription')"
    >
      <UButton
        to="/settings"
        icon="i-lucide-radio-tower"
        :label="$t('devices.configureService')"
      />
    </EmptyStatePanel>

    <UCard v-else class="mt-6">
      <div class="grid min-h-80 place-items-center">
        <UIcon name="i-lucide-loader-circle" class="size-6 animate-spin text-primary" />
      </div>
    </UCard>

    <UModal
      v-model:open="sendDialogOpen"
      :title="sendDialogTitle"
      :description="sendDialogDescription"
      :dismissible="!sending"
      :close="!sending"
      :ui="{ content: 'max-w-3xl' }"
    >
      <template #body>
        <div class="space-y-5">
          <UAlert color="warning" variant="subtle" icon="i-lucide-triangle-alert" :title="$t('devices.destructiveTitle')" :description="$t('devices.destructiveDescription')" />
          <UAlert
            v-if="sending"
            color="info"
            variant="subtle"
            icon="i-lucide-loader-circle"
            :title="$t('devices.sendingTitle')"
            :description="$t('devices.sendingProgress', sendProgress)"
            :ui="{ icon: 'animate-spin' }"
          />
          <UFormField :label="$t('devices.selectImage')">
            <USelect v-model="selectedImageId" :items="imageItems" value-key="value" class="w-full" :disabled="sending" />
          </UFormField>
          <UFormField :label="$t('devices.imageIndex')" :description="$t('devices.imageIndexHint')">
            <UInputNumber v-model="selectedImageIndex" :min="1" :max="999" class="w-full" :disabled="sending" />
          </UFormField>
          <template v-if="sendMode === 'single' && currentSendTarget">
            <UFormField :label="$t('devices.targetDisk')">
              <USelect
                :model-value="selectedDiskIds[currentSendTarget.device.id]"
                :items="targetDiskItems(currentSendTarget)"
                value-key="value"
                class="w-full"
                :disabled="sending || !isDeployableDevice(currentSendTarget)"
                @update:model-value="updateTargetDisk(currentSendTarget, $event)"
              />
            </UFormField>
            <UAlert
              v-if="!isDeployableDevice(currentSendTarget)"
              color="error"
              variant="subtle"
              icon="i-lucide-monitor-x"
              :title="$t('devices.targetUnavailableTitle')"
              :description="$t('devices.targetUnavailableHint')"
            />
          </template>
          <UAlert
            v-else-if="sendTargetCount === 0"
            color="error"
            variant="subtle"
            icon="i-lucide-monitor-x"
            :title="$t('devices.targetUnavailableTitle')"
            :description="$t('devices.targetUnavailableHint')"
          />
          <div v-else class="rounded-xl border border-default bg-elevated/40 p-4 text-sm">
            <div class="flex items-start justify-between gap-4">
              <div>
                <p class="font-medium">{{ batchTargetsTitle }}</p>
                <p class="mt-1 text-xs text-muted">{{ batchDiskHint }}</p>
              </div>
              <UBadge color="primary" variant="subtle">
                {{ batchTargetCountLabel }}
              </UBadge>
            </div>
            <div class="mt-3 max-h-64 space-y-2 overflow-y-auto pr-1">
              <div
                v-for="entry in sendTargets"
                :key="entry.device.id"
                class="grid grid-cols-[minmax(0,1fr)_minmax(240px,0.9fr)] items-center gap-4 rounded-lg border border-default bg-default p-3"
              >
                <div class="min-w-0">
                  <p class="truncate text-sm font-semibold">{{ displayName(entry) }}</p>
                  <p class="mt-1 truncate font-mono text-[11px] text-dimmed">
                    {{ entry.device.macAddress }} · {{ entry.device.ipAddress }}
                  </p>
                </div>
                <USelect
                  :model-value="selectedDiskIds[entry.device.id]"
                  :items="targetDiskItems(entry)"
                  value-key="value"
                  size="sm"
                  class="w-full"
                  :disabled="sending"
                  :aria-label="$t('devices.targetDiskFor', { name: displayName(entry) })"
                  @update:model-value="updateTargetDisk(entry, $event)"
                />
              </div>
            </div>
          </div>
          <UFormField :label="$t('devices.partitionTemplate')" :description="$t('devices.partitionTemplateHint')">
            <div class="flex gap-2">
              <USelect v-model="partitionPreset" :items="partitionPresetItems" value-key="value" class="min-w-0 flex-1" :disabled="sending" />
              <UButton icon="i-lucide-settings-2" color="neutral" variant="outline" :label="$t('devices.manageTemplates')" :disabled="sending" @click="editSelectedTemplate" />
              <UButton icon="i-lucide-plus" color="neutral" variant="outline" :aria-label="$t('devices.newTemplate')" :disabled="sending" @click="newTemplate" />
            </div>
          </UFormField>
          <UFormField
            v-if="partitionPreset === 'windows_and_data'"
            :label="$t('devices.windowsPartitionSize')"
            :description="$t('devices.windowsPartitionSizeHint')"
          >
            <UInputNumber v-model="windowsPartitionSizeGib" :min="1" :max="2048" :step="1" class="w-full" :disabled="sending" />
          </UFormField>
          <div v-if="partitionPreviewGroups.length" class="space-y-3">
            <div
              v-for="group in partitionPreviewGroups"
              :key="group.key"
              class="overflow-hidden rounded-xl border border-default"
            >
              <div class="flex items-center justify-between bg-elevated/60 px-4 py-3">
                <div>
                  <p class="text-sm font-semibold">
                    {{ $t('devices.partitionPreviewFor', { count: group.count }) }}
                  </p>
                  <p class="mt-0.5 text-xs text-muted">
                    {{ group.plan.table.toUpperCase() }} · {{ bootModeSummary(group.bootModes) }}
                  </p>
                </div>
                <UBadge color="warning" variant="subtle">{{ $t('devices.erasesDisk') }}</UBadge>
              </div>
              <div class="divide-y divide-default">
                <div v-for="(partition, partitionIndex) in group.plan.partitions" :key="`${partition.role}-${partitionIndex}`" class="flex items-center justify-between gap-4 px-4 py-3 text-sm">
                  <div class="flex items-center gap-3">
                    <UIcon :name="partition.role === 'windows' ? 'i-lucide-monitor-cog' : 'i-lucide-hard-drive'" class="size-4 text-muted" />
                    <div><p class="font-medium">{{ $t(`devices.partitionRoles.${partition.role}`) }}<template v-if="partition.driveLetter"> · {{ partition.driveLetter }}:</template></p><p class="text-xs text-muted">{{ partition.fileSystem?.toUpperCase() || '—' }}<template v-if="partition.label"> · {{ partition.label }}</template></p></div>
                  </div>
                  <p class="font-mono text-xs font-semibold">{{ partition.sizeMib === null ? $t('devices.remainingSpace') : `${partition.sizeMib} MiB` }}</p>
                </div>
              </div>
            </div>
          </div>
          <UAlert
            v-if="!selectedPlansAreKnown"
            color="error"
            variant="subtle"
            icon="i-lucide-circle-alert"
            :title="$t('devices.bootModeRequiredTitle')"
            :description="partitionPlanErrorDescription"
          />
          <UCheckbox v-model="deploymentConfirmation" :label="$t('devices.confirmDeployment')" :disabled="sending" />
        </div>
      </template>
      <template #footer>
        <div class="flex w-full justify-end gap-2">
          <UButton color="neutral" variant="outline" :label="$t('devices.cancelSend')" :disabled="sending" @click="sendDialogOpen = false" />
          <UButton
            icon="i-lucide-send"
            :label="sendActionLabel"
            :loading="sending"
            :disabled="!canSend"
            @click="handleSend"
          />
        </div>
      </template>
    </UModal>

    <UModal v-model:open="templateDialogOpen" :title="$t('devices.templateEditorTitle')" :description="$t('devices.templateEditorHint')" :ui="{ content: 'max-w-4xl' }">
      <template #body>
        <div v-if="templateDraft" class="space-y-4">
          <section class="rounded-xl border border-default bg-elevated/40 p-4">
            <div class="flex flex-wrap items-center justify-between gap-2">
              <div class="flex items-center gap-2">
                <UIcon name="i-lucide-hard-drive" class="size-4 text-primary" />
                <p class="text-sm font-semibold">{{ $t('devices.detectedDisks') }}</p>
              </div>
              <UBadge color="neutral" variant="subtle">
                {{ $t('devices.targetDeviceCount', { count: templateTargetDiskGroups.length }) }}
              </UBadge>
            </div>
            <div v-if="templateTargetDiskGroups.length" class="mt-3 grid gap-3 sm:grid-cols-2">
              <section v-for="group in templateTargetDiskGroups" :key="group.deviceId" class="overflow-hidden rounded-lg border border-default bg-default">
                <header class="border-b border-default bg-elevated/60 px-3 py-2">
                  <p class="truncate text-xs font-semibold">{{ group.deviceName }}</p>
                  <p class="mt-0.5 truncate font-mono text-[10px] text-muted">{{ group.ipAddress }} · {{ group.macAddress }}</p>
                </header>
                <div
                  v-for="row in group.disks"
                  :key="row.key"
                  class="flex min-w-0 items-center justify-between gap-3 px-3 py-2"
                  :class="row.selected ? 'bg-primary/5' : ''"
                >
                  <div class="min-w-0">
                    <p class="truncate text-xs font-medium">{{ row.disk.model || $t('devices.unknownDisk') }}</p>
                    <p class="mt-0.5 truncate font-mono text-[10px] text-muted">{{ row.disk.id }}</p>
                  </div>
                  <div class="shrink-0 text-right">
                    <p class="text-xs font-semibold">{{ formatBytes(row.disk.sizeBytes, locale) }}</p>
                    <p v-if="row.selected" class="mt-0.5 text-[10px] font-medium text-primary">{{ $t('devices.selectedTargetDisk') }}</p>
                  </div>
                </div>
              </section>
            </div>
          </section>
          <UFormField :label="$t('devices.templateName')"><UInput v-model="templateDraft.name" class="w-full" /></UFormField>
          <div class="space-y-2">
            <div v-for="(partition, index) in templateDraft.partitions" :key="index" class="grid gap-3 rounded-xl border border-default p-3 md:grid-cols-[110px_minmax(0,1fr)_140px_100px_auto_auto] md:items-end">
              <UFormField :label="$t('devices.partitionType')"><UInput :model-value="customPartitionDisplayName(partition)" disabled /></UFormField>
              <UFormField :label="$t('devices.volumeLabel')"><UInput v-model="partition.label" /></UFormField>
              <UFormField :label="$t('devices.sizeGib')">
                <UInputNumber v-if="partition.sizeGib !== null" v-model="partition.sizeGib" :min="1" :max="maximumDraftPartitionSizeGib(index)" class="w-full" />
                <UInputNumber v-else :model-value="remainingSizeGib(index)" disabled class="w-full" :placeholder="$t('devices.remainingAuto')" />
              </UFormField>
              <UFormField v-if="partition.role === 'data'" :label="$t('devices.driveLetter')"><UInput :model-value="partition.driveLetter ?? ''" maxlength="1" @update:model-value="partition.driveLetter = String($event).toUpperCase() || null" /></UFormField><div v-else />
              <UCheckbox :model-value="partition.sizeGib === null" :label="$t('devices.useRemaining')" class="pb-2" @update:model-value="setRemainingPartition(index, $event === true)" />
              <UButton icon="i-lucide-trash-2" color="error" variant="ghost" :disabled="partition.role === 'windows'" :aria-label="$t('common.delete')" @click="removeDraftPartition(index)" />
            </div>
          </div>
          <UButton icon="i-lucide-plus" color="neutral" variant="outline" :label="$t('devices.addDataPartition')" @click="addDataPartition" />
        </div>
      </template>
      <template #footer>
        <div class="flex w-full justify-between">
          <UButton color="error" variant="ghost" :label="$t('devices.deleteTemplate')" @click="deleteTemplate" />
          <div class="flex gap-2"><UButton color="neutral" variant="outline" :label="$t('common.cancel')" @click="templateDialogOpen = false" /><UButton :label="$t('devices.saveTemplate')" @click="saveTemplate" /></div>
        </div>
      </template>
    </UModal>

    <UModal v-model:open="hardwareDialogOpen" :title="$t('devices.hardwareDetails')" :description="hardwareTarget ? displayName(hardwareTarget) : ''" :ui="{ content: 'max-w-4xl' }">
      <template #body>
        <div v-if="hardwareTarget" class="space-y-5">
          <div class="grid gap-3 md:grid-cols-3">
            <div class="rounded-xl border border-default bg-elevated/40 p-4">
              <div class="flex items-center gap-2 text-muted"><UIcon name="i-lucide-cpu" class="size-4" /><span class="text-xs font-medium">CPU</span></div>
              <p class="mt-2 text-sm font-semibold">{{ hardwareTarget.device.cpuModel || $t('devices.unknownHardware') }}</p>
              <p class="mt-1 text-xs text-muted">{{ processorSummary(hardwareTarget) }}</p>
            </div>
            <div class="rounded-xl border border-default bg-elevated/40 p-4">
              <div class="flex items-center gap-2 text-muted"><UIcon name="i-lucide-memory-stick" class="size-4" /><span class="text-xs font-medium">RAM</span></div>
              <p class="mt-2 text-sm font-semibold">{{ hardwareTarget.device.memoryBytes ? formatBytes(hardwareTarget.device.memoryBytes, locale) : $t('devices.unknownHardware') }}</p>
              <p class="mt-1 text-xs text-muted">{{ $t('devices.totalMemory') }}</p>
            </div>
            <div class="rounded-xl border border-default bg-elevated/40 p-4">
              <div class="flex items-center gap-2 text-muted"><UIcon name="i-lucide-monitor-cog" class="size-4" /><span class="text-xs font-medium">{{ $t('devices.operatingSystem') }}</span></div>
              <p class="mt-2 text-sm font-semibold">{{ hardwareTarget.device.systemDetails.osName || $t('devices.unknownHardware') }}</p>
              <p class="mt-1 text-xs text-muted">{{ hardwareTarget.device.systemDetails.osVersion || '—' }} · {{ $t('devices.uptime') }} {{ formatUptime(hardwareTarget.device.systemDetails.uptimeSeconds) }}</p>
            </div>
          </div>

          <div class="grid gap-x-6 gap-y-3 rounded-xl border border-default p-4 text-xs md:grid-cols-2">
            <div><p class="text-muted">{{ $t('devices.machineModel') }}</p><p class="mt-1 font-medium">{{ hardwareTarget.device.model || '—' }}</p></div>
            <div><p class="text-muted">{{ $t('devices.machineSerial') }}</p><p class="mt-1 font-mono">{{ hardwareTarget.device.serial || '—' }}</p></div>
            <div><p class="text-muted">{{ $t('devices.motherboard') }}</p><p class="mt-1 font-medium">{{ hardwareTarget.device.systemDetails.motherboard || '—' }}</p></div>
            <div><p class="text-muted">{{ $t('devices.platform') }}</p><p class="mt-1 font-medium">{{ $t(`devices.architectures.${hardwareTarget.device.architecture}`) }} · {{ $t(`devices.bootModes.${hardwareTarget.device.bootMode}`) }}</p></div>
          </div>

          <div v-if="hardwareTarget.device.systemDetails.memoryModules.length">
            <p class="mb-2 flex items-center gap-2 text-sm font-semibold"><UIcon name="i-lucide-memory-stick" class="size-4 text-primary" />{{ $t('devices.memoryModules') }}</p>
            <div class="grid gap-2 md:grid-cols-2">
              <div v-for="(module, index) in hardwareTarget.device.systemDetails.memoryModules" :key="index" class="rounded-xl border border-default p-3">
                <p class="text-sm font-medium">{{ module.manufacturer || $t('devices.unknownHardware') }} {{ module.partNumber || '' }}</p>
                <p class="mt-1 text-xs text-muted">{{ formatBytes(module.capacityBytes, locale) }}<template v-if="module.speedMhz"> · {{ module.speedMhz }} MHz</template></p>
              </div>
            </div>
          </div>

          <div>
            <div class="mb-2 flex items-center justify-between">
              <p class="flex items-center gap-2 text-sm font-semibold"><UIcon name="i-lucide-hard-drive" class="size-4 text-primary" />{{ $t('devices.diskDetails') }}</p>
              <UBadge color="neutral" variant="subtle">{{ $t('devices.diskCount', { count: hardwareTarget.device.disks.length }) }}</UBadge>
            </div>
            <div v-if="hardwareTarget.device.disks.length" class="divide-y divide-default overflow-hidden rounded-xl border border-default">
              <div v-for="disk in hardwareTarget.device.disks" :key="disk.id" class="flex items-center justify-between gap-4 p-3">
                <div class="min-w-0"><p class="truncate text-sm font-medium">{{ disk.model || $t('devices.unknownDisk') }}</p><p class="mt-1 truncate font-mono text-[11px] text-muted">{{ disk.id }}<template v-if="disk.serial"> · {{ disk.serial }}</template></p></div>
                <div class="shrink-0 text-right"><p class="text-sm font-semibold">{{ formatBytes(disk.sizeBytes, locale) }}</p><UBadge v-if="disk.isSystem" class="mt-1" color="warning" variant="subtle" size="sm">{{ $t('devices.systemDisk') }}</UBadge></div>
              </div>
            </div>
            <p v-else class="rounded-xl border border-dashed border-default p-4 text-center text-sm text-muted">{{ $t('devices.noDisksReported') }}</p>
          </div>

          <div class="grid gap-4 md:grid-cols-2">
            <div v-if="hardwareTarget.device.systemDetails.gpus.length">
              <p class="mb-2 flex items-center gap-2 text-sm font-semibold"><UIcon name="i-lucide-gpu" class="size-4 text-primary" />{{ $t('devices.graphicsCards') }}</p>
              <div class="divide-y divide-default overflow-hidden rounded-xl border border-default">
                <div v-for="gpu in hardwareTarget.device.systemDetails.gpus" :key="gpu.name" class="p-3"><p class="text-sm font-medium">{{ gpu.name }}</p><p class="mt-1 text-xs text-muted">{{ gpu.manufacturer || '—' }}<template v-if="gpu.memoryBytes"> · {{ formatBytes(gpu.memoryBytes, locale) }}</template></p></div>
              </div>
            </div>
            <div v-if="hardwareTarget.device.systemDetails.displays.length">
              <p class="mb-2 flex items-center gap-2 text-sm font-semibold"><UIcon name="i-lucide-monitor" class="size-4 text-primary" />{{ $t('devices.displays') }}</p>
              <div class="divide-y divide-default overflow-hidden rounded-xl border border-default">
                <div v-for="display in hardwareTarget.device.systemDetails.displays" :key="display.name" class="p-3"><p class="text-sm font-medium">{{ display.name }}</p><p class="mt-1 text-xs text-muted">{{ display.manufacturer || '—' }}</p></div>
              </div>
            </div>
            <div v-if="hardwareTarget.device.systemDetails.audioDevices.length">
              <p class="mb-2 flex items-center gap-2 text-sm font-semibold"><UIcon name="i-lucide-volume-2" class="size-4 text-primary" />{{ $t('devices.audioDevices') }}</p>
              <div class="divide-y divide-default overflow-hidden rounded-xl border border-default">
                <div v-for="audio in hardwareTarget.device.systemDetails.audioDevices" :key="audio.name" class="p-3"><p class="text-sm font-medium">{{ audio.name }}</p><p class="mt-1 text-xs text-muted">{{ audio.manufacturer || '—' }}</p></div>
              </div>
            </div>
            <div v-if="hardwareTarget.device.systemDetails.networkAdapters.length">
              <p class="mb-2 flex items-center gap-2 text-sm font-semibold"><UIcon name="i-lucide-network" class="size-4 text-primary" />{{ $t('devices.networkAdapters') }}</p>
              <div class="divide-y divide-default overflow-hidden rounded-xl border border-default">
                <div v-for="adapter in hardwareTarget.device.systemDetails.networkAdapters" :key="adapter.name" class="p-3"><p class="text-sm font-medium">{{ adapter.name }}</p><p class="mt-1 text-xs text-muted"><template v-if="adapter.speedBps">{{ formatBytes(adapter.speedBps, locale) }}/s · </template>{{ adapter.macAddress || adapter.manufacturer || '—' }}</p></div>
              </div>
            </div>
          </div>
        </div>
      </template>
    </UModal>
  </div>
</template>
