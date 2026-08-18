<script setup lang="ts">
import { verifyGhoImage } from '~/services/deployment'
import { isTauriRuntime } from '~/services/runtime'
import type { ImageArtifact } from '~/types/deployment'
import { compactHash, formatBytes } from '~/utils/files'
import { classifyImageDeploymentSupport } from '~/utils/image-deployment-support'
import { detectImageOperatingSystem } from '~/utils/image-os'

definePageMeta({ titleKey: 'nav.images' })

const imageStore = useImageStore()
const toast = useToast()
const { locale, t } = useI18n()
const desktopRuntime = isTauriRuntime()

const readinessDialogOpen = ref(false)
const readinessImage = ref<ImageArtifact | null>(null)
const assessingReadiness = ref(false)
const readinessError = ref<string | null>(null)

const imageRows = computed(() => imageStore.images.map(image => ({
  image,
  deploymentSupport: classifyImageDeploymentSupport(image.format, image.verified)
})))

function formatImportedAt(value: string) {
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'short'
  }).format(new Date(value))
}

function openReadinessDialog(image: ImageArtifact) {
  readinessImage.value = image
  readinessError.value = null
  readinessDialogOpen.value = true
}

async function runReadinessAssessment() {
  const image = readinessImage.value
  if (!image || assessingReadiness.value) return

  assessingReadiness.value = true
  readinessError.value = null
  try {
    const verified = await verifyGhoImage(image.id)
    await imageStore.refresh()
    readinessImage.value = imageStore.images.find(value => value.id === image.id) ?? image
    toast.add({
      title: verified.ghoCapability?.deployable
        ? t('images.ghoReadiness.verificationComplete')
        : t('images.ghoReadiness.verificationBlocked'),
      description: verified.ghoCapability?.blockedReason ?? undefined,
      color: verified.ghoCapability?.deployable ? 'success' : 'warning',
      icon: verified.ghoCapability?.deployable
        ? 'i-lucide-badge-check'
        : 'i-lucide-circle-alert'
    })
  } catch (error) {
    readinessError.value = String(error)
  } finally {
    assessingReadiness.value = false
  }
}

async function handleImport() {
  if (!isTauriRuntime()) {
    toast.add({
      title: t('images.desktopOnly'),
      color: 'neutral',
      icon: 'i-lucide-monitor-up'
    })
    return
  }

  try {
    const imported = await imageStore.importFromPicker()
    if (imported > 0) {
      toast.add({
        title: t('images.imported', { count: imported }),
        color: 'success',
        icon: 'i-lucide-badge-check'
      })
    }
  } catch {
    toast.add({
      title: t('images.importFailed'),
      description: imageStore.lastError ?? undefined,
      color: 'error',
      icon: 'i-lucide-circle-alert'
    })
  }
}

async function handleRemove(image: ImageArtifact) {
  if (!window.confirm(t('images.removeConfirm', { name: image.name }))) {
    return
  }

  try {
    await imageStore.remove(image.id)
    toast.add({
      title: t('images.removed'),
      color: 'success',
      icon: 'i-lucide-trash-2'
    })
  } catch {
    toast.add({
      title: t('images.removeFailed'),
      description: imageStore.lastError ?? undefined,
      color: 'error',
      icon: 'i-lucide-circle-alert'
    })
  }
}
</script>

<template>
  <div class="mx-auto max-w-[1500px] p-6 lg:p-8">
    <PageHeader
      :title="$t('images.title')"
      :description="$t('images.description')"
    >
      <template #actions>
        <UButton
          icon="i-lucide-file-plus-2"
          :label="$t('images.import')"
          :loading="imageStore.importing"
          @click="handleImport"
        />
      </template>
    </PageHeader>

    <UAlert
      v-if="imageStore.lastError"
      class="mt-6"
      color="error"
      variant="subtle"
      icon="i-lucide-circle-alert"
      :title="$t('images.importFailed')"
      :description="imageStore.lastError"
    />

    <UCard
      v-if="imageRows.length"
      class="mt-6 overflow-hidden ring-1 ring-default/70"
      :ui="{ header: 'px-6 py-5 sm:px-6', body: 'p-0 sm:p-0' }"
    >
      <template #header>
        <div class="flex items-center justify-between gap-4">
          <div class="flex min-w-0 items-center gap-3">
            <div class="grid size-9 shrink-0 place-items-center rounded-lg bg-primary/10 text-primary ring-1 ring-primary/15">
              <UIcon name="i-lucide-layers-3" class="size-4.5" />
            </div>
            <div class="min-w-0">
              <h2 class="text-base font-semibold text-highlighted">
                {{ $t('images.available') }}
              </h2>
              <p class="mt-0.5 truncate text-sm text-muted">
                {{ $t('images.availableHint') }}
              </p>
            </div>
          </div>
          <UBadge class="shrink-0" color="success" variant="subtle" size="md">
            {{ $t('images.verifiedCount', { count: imageStore.verifiedCount }) }}
          </UBadge>
        </div>
      </template>

      <div class="divide-y divide-default/70">
        <article
          v-for="{ image, deploymentSupport } in imageRows"
          :key="image.id"
          class="group flex items-stretch transition-colors hover:bg-elevated/40"
        >
          <div class="flex min-w-0 flex-1 items-center gap-4 px-6 py-4">
            <ImageOsLogo
              :operating-system="detectImageOperatingSystem(image.name, image.sourcePath)"
            />
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <p class="truncate text-sm font-semibold leading-5 text-highlighted" :title="image.name">
                  {{ image.name }}
                </p>
                <UIcon
                  v-if="image.verified"
                  name="i-lucide-badge-check"
                  class="size-4 shrink-0 text-success"
                />
                <UBadge
                  class="shrink-0"
                  :color="deploymentSupport === 'automatic'
                    ? 'success'
                    : deploymentSupport === 'manual'
                      ? 'info'
                    : deploymentSupport === 'verification-required'
                      ? 'neutral'
                      : 'warning'"
                  variant="subtle"
                  size="sm"
                  :title="$t(deploymentSupport === 'automatic'
                    ? 'images.automaticDeploymentHint'
                    : deploymentSupport === 'manual'
                      ? 'images.manualDeploymentHint'
                    : deploymentSupport === 'verification-required'
                      ? 'images.verificationRequiredHint'
                      : 'images.catalogOnlyHint')"
                >
                  {{ $t(deploymentSupport === 'automatic'
                    ? 'images.automaticDeployment'
                    : deploymentSupport === 'manual'
                      ? 'images.manualDeployment'
                    : deploymentSupport === 'verification-required'
                      ? 'images.verificationRequired'
                      : 'images.catalogOnly') }}
                </UBadge>
              </div>
              <p
                class="mt-1 truncate font-mono text-[11px] text-dimmed"
                :title="image.sourcePath"
              >
                {{ image.sourcePath }}
              </p>
            </div>

            <dl class="ml-3 grid shrink-0 grid-cols-[72px_92px_180px] items-center gap-x-5">
              <div>
                <dt class="text-[10px] font-semibold uppercase tracking-[0.08em] text-dimmed">
                  {{ $t('common.format') }}
                </dt>
                <dd class="mt-1.5">
                  <UBadge color="neutral" variant="outline" size="sm">
                    {{ image.format.toUpperCase() }}
                  </UBadge>
                </dd>
              </div>

              <div>
                <dt class="text-[10px] font-semibold uppercase tracking-[0.08em] text-dimmed">
                  {{ $t('common.size') }}
                </dt>
                <dd class="mt-1.5 whitespace-nowrap text-sm font-semibold text-highlighted">
                  {{ formatBytes(image.sizeBytes, locale) }}
                </dd>
              </div>

              <div class="min-w-0">
                <dt class="text-[10px] font-semibold uppercase tracking-[0.08em] text-dimmed">
                  {{ $t('images.digest') }}
                </dt>
                <dd
                  class="mt-1.5 truncate font-mono text-[11px] text-muted"
                  :title="image.sha256 ?? ''"
                >
                  {{ compactHash(image.sha256) }}
                </dd>
                <dd class="mt-0.5 truncate text-[10px] text-dimmed">
                  {{ image.spans.length
                    ? $t('images.spanCount', { count: image.spans.length })
                    : formatImportedAt(image.createdAt) }}
                </dd>
              </div>
            </dl>
          </div>

          <div class="flex w-24 shrink-0 items-center justify-center gap-1 border-l border-default/60">
            <UButton
              v-if="image.format === 'gho'"
              class="opacity-70 transition-opacity group-hover:opacity-100"
              icon="i-lucide-shield-check"
              color="neutral"
              variant="ghost"
              size="sm"
              square
              :disabled="!desktopRuntime"
              :aria-label="$t('images.ghoReadiness.open')"
              :title="$t('images.ghoReadiness.open')"
              @click="openReadinessDialog(image)"
            />
            <UButton
              class="opacity-70 transition-opacity group-hover:opacity-100"
              icon="i-lucide-trash-2"
              color="error"
              variant="ghost"
              size="sm"
              square
              :aria-label="$t('common.delete')"
              :title="$t('common.delete')"
              @click="handleRemove(image)"
            />
          </div>
        </article>
      </div>
    </UCard>

    <EmptyStatePanel
      v-else-if="!imageStore.loading"
      icon="i-lucide-layers-3"
      :title="$t('images.emptyTitle')"
      :description="$t('images.emptyDescription')"
    >
      <UButton
        icon="i-lucide-file-plus-2"
        :label="$t('images.import')"
        :loading="imageStore.importing"
        @click="handleImport"
      />
    </EmptyStatePanel>

    <UCard v-else class="mt-6">
      <div class="grid min-h-80 place-items-center">
        <UIcon name="i-lucide-loader-circle" class="size-6 animate-spin text-primary" />
      </div>
    </UCard>

    <UModal
      v-model:open="readinessDialogOpen"
      :title="$t('images.ghoReadiness.title')"
      :description="readinessImage
        ? $t('images.ghoReadiness.description', { name: readinessImage.name })
        : ''"
    >
      <template #body>
        <div class="space-y-5">
          <UAlert
            :color="readinessImage?.ghoCapability?.deployable ? 'success' : 'warning'"
            variant="subtle"
            icon="i-lucide-shield-check"
            :title="$t('images.ghoReadiness.nativeTitle')"
            :description="$t('images.ghoReadiness.nativeDescription')"
          />
          <UAlert
            v-if="readinessError"
            color="error"
            variant="subtle"
            icon="i-lucide-circle-alert"
            :title="$t('images.ghoReadiness.assessmentFailed')"
            :description="readinessError"
          />

          <div v-if="readinessImage?.ghoCapability" class="rounded-xl border border-default p-4">
            <dl class="grid gap-3 text-xs md:grid-cols-2">
              <div><dt class="font-medium text-muted">{{ $t('common.status') }}</dt><dd class="mt-1">{{ readinessImage.ghoCapability.deployable ? $t('images.manualDeployment') : readinessImage.ghoCapability.blockedReason }}</dd></div>
              <div v-if="readinessImage.ghoCapability.compression"><dt class="font-medium text-muted">{{ $t('images.ghoReadiness.compression') }}</dt><dd class="mt-1">{{ readinessImage.ghoCapability.compression.toUpperCase() }}</dd></div>
              <div v-if="readinessImage.ghoCapability.expandedSizeBytes">
                <dt class="font-medium text-muted">{{ $t('images.ghoReadiness.expandedSize') }}</dt>
                <dd class="mt-1">{{ formatBytes(readinessImage.ghoCapability.expandedSizeBytes, locale) }}</dd>
              </div>
              <div v-if="readinessImage.ghoCapability.expandedSha256">
                <dt class="font-medium text-muted">{{ $t('images.ghoReadiness.expandedHash') }}</dt>
                <dd class="mt-1 break-all font-mono text-[10px]">{{ readinessImage.ghoCapability.expandedSha256 }}</dd>
              </div>
            </dl>
          </div>
        </div>
      </template>
      <template #footer>
        <div class="flex w-full justify-end gap-2">
          <UButton
            color="neutral"
            variant="outline"
            :label="$t('images.ghoReadiness.close')"
            @click="readinessDialogOpen = false"
          />
          <UButton
            icon="i-lucide-shield-check"
            :label="$t('images.ghoReadiness.run')"
            :loading="assessingReadiness"
            :disabled="!desktopRuntime || assessingReadiness"
            @click="runReadinessAssessment"
          />
        </div>
      </template>
    </UModal>
  </div>
</template>
