<script setup lang="ts">
import { isTauriRuntime } from '~/services/runtime'
import { importPxeBootPackage, importPxeMedia, loadPxeConfig, savePxeConfig } from '~/services/runtime'
import { open } from '@tauri-apps/plugin-dialog'
import type { PxeConfig, PxeMode } from '~/types/runtime'
import { preferredIpv4Interface, suggestedPxeNetwork } from '~/utils/network'
import { controlStartNeedsPe, parseSettingsDraft, pxeSourceDisplayName, settingsDraftStorageKey, type SettingsDraft } from '~/utils/settings'

definePageMeta({ titleKey: 'nav.settings' })

const { currentLocale, changeLocale } = useAppLocale()
const runtimeStore = useRuntimeStore()
const toast = useToast()
const { t } = useI18n()

const languageItems = [
  { label: '简体中文', value: 'zh-CN' },
  { label: 'English', value: 'en-US' }
]
const selectedLocale = computed({
  get: () => currentLocale.value,
  set: value => changeLocale(value)
})

const bindAddress = ref('')
const port = ref(7760)
const revealToken = ref(false)
const pxeMode = ref<PxeMode>('standalone_dhcp')
const subnetMask = ref('255.255.255.0')
const poolStart = ref('')
const poolEnd = ref('')
const leaseSeconds = ref(28_800)
const gateway = ref('')
const dnsServers = ref('')
const tftpRoot = ref('')
const peName = ref('')
const biosBootFile = ref('undionly.kpxe')
const uefiX64BootFile = ref('ipxe.efi')
const pxeConfigLoaded = ref(false)
const settingsDraftLoaded = ref(false)
const isImportingPxeMedia = ref(false)
const peSetupError = ref(false)
const peSetupSection = ref<HTMLElement | null>(null)
const ipv4Interfaces = computed(() =>
  runtimeStore.usableInterfaces.filter(item => !item.address.includes(':'))
)
const interfaceItems = computed(() =>
  ipv4Interfaces.value.map(item => ({
    label: `${item.name} — ${item.address}`,
    value: item.address
  }))
)
const selectedIpv4Interface = computed(() =>
  ipv4Interfaces.value.find(item => item.address === bindAddress.value) ?? null
)
const isRunning = computed(
  () => runtimeStore.controlStatus.state === 'running'
)
const isPxeRunning = computed(() => runtimeStore.pxeStatus.state === 'running')
const pxeConfig = computed<PxeConfig>(() => ({
  mode: pxeMode.value,
  bindAddress: bindAddress.value,
  subnetMask: subnetMask.value,
  poolStart: poolStart.value,
  poolEnd: poolEnd.value,
  leaseSeconds: leaseSeconds.value,
  gateway: gateway.value || null,
  dnsServers: dnsServers.value.split(',').map(value => value.trim()).filter(Boolean),
  tftpRoot: tftpRoot.value,
  biosBootFile: biosBootFile.value,
  uefiX64BootFile: uefiX64BootFile.value
}))
const settingsDraft = computed<SettingsDraft>(() => ({
  bindAddress: bindAddress.value,
  port: port.value,
  pxeMode: pxeMode.value,
  subnetMask: subnetMask.value,
  poolStart: poolStart.value,
  poolEnd: poolEnd.value,
  leaseSeconds: leaseSeconds.value,
  gateway: gateway.value,
  dnsServers: dnsServers.value,
  tftpRoot: tftpRoot.value,
  peName: peName.value,
  biosBootFile: biosBootFile.value,
  uefiX64BootFile: uefiX64BootFile.value
}))
const agentCommand = computed(() => {
  const status = runtimeStore.controlStatus
  if (!status.endpoint || !status.enrollmentToken) {
    return ''
  }
  return `easydeploymesh-agent --server ${status.endpoint} --enrollment-token ${status.enrollmentToken}`
})

watch(tftpRoot, (root) => {
  if (root) peSetupError.value = false
})

async function guideToPeSetup() {
  peSetupError.value = true
  toast.add({
    title: t('settings.peRequiredForControl'),
    description: t('settings.peRequiredForControlHint'),
    color: 'warning',
    icon: 'i-lucide-disc-3'
  })
  await nextTick()
  peSetupSection.value?.scrollIntoView({ behavior: 'smooth', block: 'center' })
  peSetupSection.value?.querySelector<HTMLElement>('[data-pe-import]')?.focus()
}

function applyDetectedNetwork(showToast = false) {
  const selected = selectedIpv4Interface.value
  const suggestion = selected && suggestedPxeNetwork(selected)
  if (!selected || !suggestion) {
    if (showToast) toast.add({ title: t('settings.pxeAutoDetectFailed'), color: 'warning', icon: 'i-lucide-circle-alert' })
    return
  }
  subnetMask.value = suggestion.subnetMask
  poolStart.value = suggestion.poolStart
  poolEnd.value = suggestion.poolEnd
  if (showToast) toast.add({ title: t('settings.pxeNetworkDetected'), description: `${selected.name} — ${selected.address}`, color: 'success', icon: 'i-lucide-wifi' })
}

watch(ipv4Interfaces, (interfaces) => {
  if (!bindAddress.value) bindAddress.value = preferredIpv4Interface(interfaces)?.address ?? ''
  if (pxeConfigLoaded.value && !poolStart.value && !poolEnd.value) applyDetectedNetwork()
}, { immediate: true })

watch(bindAddress, () => {
  if (pxeConfigLoaded.value && !isPxeRunning.value) applyDetectedNetwork()
})

watch(settingsDraft, (draft) => {
  if (settingsDraftLoaded.value && import.meta.client) {
    localStorage.setItem(settingsDraftStorageKey, JSON.stringify(draft))
  }
}, { deep: true })

onMounted(async () => {
  const draft = parseSettingsDraft(localStorage.getItem(settingsDraftStorageKey))
  const saved = draft ? null : await loadPxeConfig()
  if (draft) {
    bindAddress.value = draft.bindAddress
    port.value = draft.port
    pxeMode.value = draft.pxeMode
    subnetMask.value = draft.subnetMask
    poolStart.value = draft.poolStart
    poolEnd.value = draft.poolEnd
    leaseSeconds.value = draft.leaseSeconds
    gateway.value = draft.gateway
    dnsServers.value = draft.dnsServers
    tftpRoot.value = draft.tftpRoot
    peName.value = draft.peName
    biosBootFile.value = draft.biosBootFile
    uefiX64BootFile.value = draft.uefiX64BootFile
  } else if (saved) {
    pxeMode.value = saved.mode
    bindAddress.value = saved.bindAddress
    subnetMask.value = saved.subnetMask
    poolStart.value = saved.poolStart
    poolEnd.value = saved.poolEnd
    leaseSeconds.value = saved.leaseSeconds
    gateway.value = saved.gateway || ''
    dnsServers.value = saved.dnsServers.join(', ')
    tftpRoot.value = saved.tftpRoot
    biosBootFile.value = saved.biosBootFile
    uefiX64BootFile.value = saved.uefiX64BootFile
  } else {
    const preferred = preferredIpv4Interface(ipv4Interfaces.value)
    if (preferred) bindAddress.value = preferred.address
    applyDetectedNetwork()
  }
  pxeConfigLoaded.value = true
  settingsDraftLoaded.value = true
  localStorage.setItem(settingsDraftStorageKey, JSON.stringify(settingsDraft.value))
})

async function handleImportBootPackage() {
  const source = await open({ directory: true, multiple: false })
  if (!source || Array.isArray(source)) return
  try {
    const imported = await importPxeBootPackage(source, biosBootFile.value, uefiX64BootFile.value)
    tftpRoot.value = imported.root
    peName.value = pxeSourceDisplayName(source)
    toast.add({ title: t('settings.pxePackageImported'), color: 'success', icon: 'i-lucide-package-check' })
  } catch (error) {
    toast.add({ title: t('settings.pxeImportFailed'), description: String(error), color: 'error', icon: 'i-lucide-circle-alert' })
  }
}

async function handleImportMedia() {
  if (isImportingPxeMedia.value) return
  const source = await open({ multiple: false, filters: [{ name: 'PE boot media', extensions: ['iso', 'img'] }] })
  if (!source || Array.isArray(source)) return
  isImportingPxeMedia.value = true
  try {
    const imported = await importPxeMedia(source)
    tftpRoot.value = imported.root
    peName.value = pxeSourceDisplayName(source)
    biosBootFile.value = imported.biosBootFile
    uefiX64BootFile.value = imported.uefiX64BootFile
    peSetupError.value = false
    toast.add({ title: t('settings.pxeMediaImported'), description: imported.biosBootFile ? undefined : t('settings.pxeBiosUnavailable'), color: imported.biosBootFile ? 'success' : 'warning', icon: 'i-lucide-disc-3' })
  } catch (error) {
    toast.add({ title: t('settings.pxeImportFailed'), description: String(error), color: 'error', icon: 'i-lucide-circle-alert' })
  } finally {
    isImportingPxeMedia.value = false
  }
}

async function handleStartPxe() {
  try {
    await savePxeConfig(pxeConfig.value)
    await runtimeStore.startPxe(pxeConfig.value, port.value)
    toast.add({ title: t('settings.pxeStarted'), color: 'success', icon: 'i-lucide-network' })
  } catch (error) {
    toast.add({ title: t('settings.pxeStartFailed'), description: String(error), color: 'error', icon: 'i-lucide-circle-alert' })
  }
}

async function handleStopPxe() {
  try { await runtimeStore.stopPxe() }
  catch (error) { toast.add({ title: t('settings.pxeStopFailed'), description: String(error), color: 'error', icon: 'i-lucide-circle-alert' }) }
}

async function handleStart() {
  if (!isTauriRuntime()) {
    toast.add({
      title: t('settings.desktopOnly'),
      color: 'neutral',
      icon: 'i-lucide-monitor-up'
    })
    return
  }
  if (!bindAddress.value) {
    toast.add({
      title: t('settings.selectInterface'),
      color: 'warning',
      icon: 'i-lucide-circle-alert'
    })
    return
  }
  try {
    await runtimeStore.start(bindAddress.value, port.value)
    toast.add({
      title: t('settings.serviceStarted'),
      color: 'success',
      icon: 'i-lucide-radio-tower'
    })
  } catch (error) {
    if (controlStartNeedsPe(tftpRoot.value, error)) {
      tftpRoot.value = ''
      peName.value = ''
      await guideToPeSetup()
      return
    }
    toast.add({
      title: t('errors.controlStartFailed'),
      description: String(error),
      color: 'error',
      icon: 'i-lucide-circle-alert'
    })
  }
}

async function handleStop() {
  try {
    await runtimeStore.stop()
    revealToken.value = false
    toast.add({
      title: t('settings.serviceStopped'),
      color: 'neutral',
      icon: 'i-lucide-circle-stop'
    })
  } catch (error) {
    toast.add({
      title: t('errors.controlStopFailed'),
      description: String(error),
      color: 'error',
      icon: 'i-lucide-circle-alert'
    })
  }
}

async function copyAgentCommand() {
  if (!agentCommand.value) {
    return
  }
  try {
    await navigator.clipboard.writeText(agentCommand.value)
    toast.add({
      title: t('common.copied'),
      color: 'success',
      icon: 'i-lucide-copy-check'
    })
  } catch {
    toast.add({
      title: t('common.copyFailed'),
      color: 'error',
      icon: 'i-lucide-circle-alert'
    })
  }
}
</script>

<template>
  <div class="mx-auto max-w-5xl space-y-6 p-6 lg:p-8">
    <PageHeader
      :title="$t('settings.title')"
      :description="$t('settings.description')"
    />

    <UCard>
      <template #header>
        <div>
          <h2 class="text-base font-semibold">
            {{ $t('settings.appearance') }}
          </h2>
          <p class="mt-1 text-sm text-muted">
            {{ $t('settings.appearanceHint') }}
          </p>
        </div>
      </template>

      <div class="flex items-center justify-between gap-8">
        <div>
          <p class="text-sm font-medium">
            {{ $t('settings.preferredLanguage') }}
          </p>
          <p class="mt-1 text-xs text-muted">
            zh-CN / en-US
          </p>
        </div>
        <USelect
          v-model="selectedLocale"
          :items="languageItems"
          value-key="value"
          class="w-52"
          icon="i-lucide-languages"
        />
      </div>
    </UCard>

    <UCard id="network" class="scroll-mt-6">
      <template #header>
        <div class="flex items-center justify-between gap-4">
          <div>
            <h2 class="text-base font-semibold">
              {{ $t('settings.network') }}
            </h2>
            <p class="mt-1 text-sm text-muted">
              {{ $t('settings.networkHint') }}
            </p>
          </div>
          <UBadge
            :color="isRunning ? 'success' : 'neutral'"
            variant="subtle"
          >
            {{ $t(`common.${runtimeStore.controlStatus.state}`) }}
          </UBadge>
        </div>
      </template>

      <div class="grid grid-cols-2 gap-8">
        <section>
          <h3 class="text-sm font-semibold">
            {{ $t('settings.listenConfiguration') }}
          </h3>
          <p class="mt-1 text-xs leading-5 text-muted">
            {{ $t('settings.listenConfigurationHint') }}
          </p>

          <div class="mt-5 space-y-4">
            <UFormField
              :label="$t('settings.deploymentInterface')"
              :description="$t('settings.deploymentInterfaceHint')"
            >
              <USelect
                v-model="bindAddress"
                :items="interfaceItems"
                value-key="value"
                class="w-full"
                icon="i-lucide-ethernet-port"
                :disabled="isRunning"
                :placeholder="$t('settings.selectInterface')"
              />
            </UFormField>

            <UFormField
              :label="$t('settings.controlPort')"
              :description="$t('settings.controlPortHint')"
            >
              <UInputNumber
                v-model="port"
                class="w-full"
                :min="1024"
                :max="65535"
                :disabled="isRunning"
              />
            </UFormField>

            <div class="flex gap-3 pt-1">
              <UButton
                v-if="!isRunning"
                icon="i-lucide-play"
                :label="$t(runtimeStore.controlLoading ? 'settings.startingService' : 'settings.startService')"
                :loading="runtimeStore.controlLoading"
                :disabled="!bindAddress"
                @click="handleStart"
              />
              <UButton
                v-else
                icon="i-lucide-square"
                color="error"
                variant="soft"
                :label="$t(runtimeStore.controlLoading ? 'settings.stoppingService' : 'settings.stopService')"
                :loading="runtimeStore.controlLoading"
                @click="handleStop"
              />
            </div>
          </div>
        </section>

        <section class="rounded-xl border border-default bg-elevated/40 p-5">
          <div v-if="isRunning" class="space-y-5">
            <div class="flex items-center gap-3">
              <span class="relative flex size-2.5">
                <span class="absolute inline-flex size-full animate-ping rounded-full bg-success opacity-40" />
                <span class="relative inline-flex size-2.5 rounded-full bg-success" />
              </span>
              <div>
                <p class="text-sm font-semibold">
                  {{ $t('settings.acceptingAgents') }}
                </p>
                <p class="mt-0.5 font-mono text-xs text-muted">
                  {{ runtimeStore.controlStatus.endpoint }}
                </p>
              </div>
            </div>

            <div>
              <div class="flex items-center justify-between gap-3">
                <p class="text-xs font-medium text-muted">
                  {{ $t('settings.enrollmentToken') }}
                </p>
                <UButton
                  :icon="revealToken ? 'i-lucide-eye-off' : 'i-lucide-eye'"
                  color="neutral"
                  variant="ghost"
                  size="xs"
                  :aria-label="$t('settings.toggleToken')"
                  @click="revealToken = !revealToken"
                />
              </div>
              <code class="mt-2 block break-all rounded-lg border border-default bg-default p-3 text-xs leading-5">
                {{ revealToken
                  ? runtimeStore.controlStatus.enrollmentToken
                  : '••••••••••••••••••••••••••••••••' }}
              </code>
              <p class="mt-2 text-[11px] leading-4 text-dimmed">
                {{ $t('settings.enrollmentTokenHint') }}
              </p>
            </div>

            <div>
              <div class="flex items-center justify-between gap-3">
                <p class="text-xs font-medium text-muted">
                  {{ $t('settings.agentCommand') }}
                </p>
                <UButton
                  icon="i-lucide-copy"
                  color="neutral"
                  variant="ghost"
                  size="xs"
                  :label="$t('common.copy')"
                  @click="copyAgentCommand"
                />
              </div>
              <code class="mt-2 block max-h-24 overflow-auto break-all rounded-lg border border-default bg-default p-3 text-[11px] leading-5">
                {{ agentCommand }}
              </code>
            </div>
          </div>

          <div v-else class="grid min-h-72 place-items-center text-center">
            <div>
              <div class="mx-auto grid size-12 place-items-center rounded-xl bg-default ring-1 ring-default">
                <UIcon name="i-lucide-radio-tower" class="size-5 text-muted" />
              </div>
              <p class="mt-4 text-sm font-medium">
                {{ $t('settings.serviceIdle') }}
              </p>
              <p class="mx-auto mt-1 max-w-xs text-xs leading-5 text-muted">
                {{ $t('settings.serviceIdleHint') }}
              </p>
            </div>
          </div>
        </section>
      </div>
    </UCard>

    <UCard>
      <template #header>
        <div class="flex items-center justify-between gap-4">
          <div>
            <h2 class="text-base font-semibold">{{ $t('settings.pxeTitle') }}</h2>
            <p class="mt-1 text-sm text-muted">{{ $t('settings.pxeHint') }}</p>
          </div>
          <UBadge :color="isPxeRunning ? 'success' : 'neutral'" variant="subtle">
            {{ $t(`common.${runtimeStore.pxeStatus.state}`) }}
          </UBadge>
        </div>
      </template>

      <div class="grid grid-cols-2 gap-8">
        <section class="space-y-4">
          <div class="flex items-center justify-between gap-3 rounded-lg border border-default bg-elevated/40 px-3 py-2.5">
            <div class="min-w-0">
              <p class="text-xs font-medium">{{ $t('settings.pxeDetectedInterface') }}</p>
              <p class="truncate font-mono text-[11px] text-muted">
                {{ selectedIpv4Interface ? `${selectedIpv4Interface.name} — ${selectedIpv4Interface.address}` : $t('settings.pxeNoInterface') }}
              </p>
            </div>
            <UButton icon="i-lucide-scan-search" color="neutral" variant="outline" size="xs" :disabled="isPxeRunning || !selectedIpv4Interface" :label="$t('settings.pxeAutoFill')" @click="applyDetectedNetwork(true)" />
          </div>
          <UFormField :label="$t('settings.pxeMode')">
            <USelect v-model="pxeMode" :disabled="isPxeRunning" value-key="value" :items="[
              { label: $t('settings.standaloneDhcp'), value: 'standalone_dhcp' },
              { label: $t('settings.proxyDhcp'), value: 'proxy_dhcp' }
            ]" class="w-full" />
          </UFormField>
          <div class="grid grid-cols-2 gap-3">
            <UFormField :label="$t('settings.subnetMask')"><UInput v-model="subnetMask" :disabled="isPxeRunning" /></UFormField>
            <UFormField :label="$t('settings.leaseSeconds')"><UInputNumber v-model="leaseSeconds" :min="60" :max="604800" :disabled="isPxeRunning" /></UFormField>
            <UFormField :label="$t('settings.poolStart')"><UInput v-model="poolStart" :disabled="isPxeRunning" /></UFormField>
            <UFormField :label="$t('settings.poolEnd')"><UInput v-model="poolEnd" :disabled="isPxeRunning" /></UFormField>
            <UFormField :label="$t('settings.gateway')"><UInput v-model="gateway" :disabled="isPxeRunning" placeholder="Optional" /></UFormField>
            <UFormField :label="$t('settings.dnsServers')"><UInput v-model="dnsServers" :disabled="isPxeRunning" placeholder="1.1.1.1, 8.8.8.8" /></UFormField>
          </div>
          <p class="text-[11px] leading-4 text-dimmed">{{ $t('settings.pxeManualEditHint') }}</p>
        </section>

        <section
          id="boot-pack"
          ref="peSetupSection"
          class="scroll-mt-6 space-y-4 rounded-xl border p-5 transition-colors"
          :class="peSetupError
            ? 'border-error bg-error/5 ring-2 ring-error/20'
            : tftpRoot
              ? 'border-success/30 bg-success/5'
              : 'border-default bg-elevated/40'"
        >
          <UAlert
            v-if="peSetupError"
            color="error"
            variant="subtle"
            icon="i-lucide-circle-alert"
            :title="$t('settings.peRequiredForControl')"
            :description="$t('settings.peRequiredForControlHint')"
          />
          <div v-if="tftpRoot" class="relative overflow-hidden rounded-xl border border-success/25 bg-default/80 p-4 shadow-sm">
            <div class="absolute inset-y-0 left-0 w-1 bg-success" />
            <div class="flex items-start gap-3">
            <div class="grid size-10 shrink-0 place-items-center rounded-xl bg-success/10 text-success ring-1 ring-success/15">
              <UIcon name="i-lucide-circle-check-big" class="size-5" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex items-start justify-between gap-3">
                <div class="min-w-0">
                  <p class="text-[11px] font-medium uppercase tracking-wide text-success">{{ $t('settings.currentPe') }}</p>
                  <p class="mt-0.5 truncate text-base font-semibold text-highlighted" :title="peName || $t('settings.pxePackageReady')">
                    {{ peName || $t('settings.pxePackageReady') }}
                  </p>
                </div>
                <UBadge color="success" variant="subtle" size="sm" class="shrink-0">{{ $t('settings.ready') }}</UBadge>
              </div>
              <p class="mt-1 text-xs text-muted">{{ $t('settings.pxePackageReadyHint') }}</p>
              <div class="mt-3 flex flex-wrap gap-1.5">
                <UBadge v-if="biosBootFile" color="neutral" variant="subtle" size="sm">Legacy BIOS</UBadge>
                <UBadge v-if="uefiX64BootFile" color="neutral" variant="subtle" size="sm">UEFI x64</UBadge>
              </div>
            </div>
            </div>
          </div>
          <div v-else class="rounded-xl border border-dashed border-default bg-default/50 p-5 text-center">
            <div class="mx-auto grid size-10 place-items-center rounded-full bg-elevated text-muted">
              <UIcon name="i-lucide-package-open" class="size-5" />
            </div>
            <p class="mt-3 text-sm font-medium">{{ $t('settings.noPxePackage') }}</p>
            <p class="mt-1 text-xs text-muted">{{ $t('settings.noPxePackageHint') }}</p>
          </div>
          <UFormField :label="$t('settings.biosBootFile')"><UInput v-model="biosBootFile" :disabled="isPxeRunning" /></UFormField>
          <UFormField :label="$t('settings.uefiBootFile')"><UInput v-model="uefiX64BootFile" :disabled="isPxeRunning" /></UFormField>
          <UAlert v-if="tftpRoot && !biosBootFile" color="warning" variant="subtle" :title="$t('settings.pxeBiosUnavailable')" />
          <div v-if="tftpRoot" class="rounded-lg border border-default bg-default/70 px-3 py-2.5">
            <p class="text-[11px] font-medium text-muted">{{ $t('settings.managedPackagePath') }}</p>
            <p class="mt-1 break-all font-mono text-[11px] text-dimmed">{{ tftpRoot }}</p>
          </div>
          <div class="flex flex-wrap gap-2">
            <UButton icon="i-lucide-folder-input" color="neutral" variant="outline" size="sm" :disabled="isPxeRunning || isImportingPxeMedia" :label="$t(tftpRoot ? 'settings.replacePxePackage' : 'settings.importPxePackage')" @click="handleImportBootPackage" />
            <UButton data-pe-import icon="i-lucide-disc-3" :color="peSetupError ? 'error' : 'neutral'" :variant="peSetupError ? 'solid' : 'outline'" size="sm" :loading="isImportingPxeMedia" :disabled="isPxeRunning || isImportingPxeMedia" :label="$t(tftpRoot ? 'settings.replacePxeMedia' : 'settings.importPxeMedia')" @click="handleImportMedia" />
          </div>
          <div v-if="isPxeRunning" class="text-xs text-muted">
            DHCP: {{ runtimeStore.pxeStatus.dhcpPort || runtimeStore.pxeStatus.proxyDhcpPort }} · TFTP: {{ runtimeStore.pxeStatus.tftpPort }} · {{ $t('settings.activeLeases') }}: {{ runtimeStore.pxeStatus.activeLeases }}
          </div>
          <UButton v-if="!isPxeRunning" icon="i-lucide-play" block :loading="runtimeStore.pxeLoading" :disabled="!bindAddress || !tftpRoot || isImportingPxeMedia" :label="$t(runtimeStore.pxeLoading ? 'settings.startingPxe' : 'settings.startPxe')" @click="handleStartPxe" />
          <UButton v-else icon="i-lucide-square" color="error" variant="soft" :loading="runtimeStore.pxeLoading" :label="$t(runtimeStore.pxeLoading ? 'settings.stoppingPxe' : 'settings.stopPxe')" @click="handleStopPxe" />
        </section>
      </div>
    </UCard>

    <UAlert
      color="warning"
      variant="subtle"
      icon="i-lucide-shield-alert"
      :title="$t('settings.securityTitle')"
      :description="$t('settings.securityDescription')"
    />
  </div>
</template>
