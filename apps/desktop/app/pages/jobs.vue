<script setup lang="ts">
import type { JobState } from '~/types/deployment'

definePageMeta({ titleKey: 'nav.jobs' })

const jobStore = useJobStore()
const toast = useToast()
const { locale, t } = useI18n()
const deletingJobId = ref<string | null>(null)
let refreshTimer: ReturnType<typeof setInterval> | undefined

onMounted(async () => {
  await jobStore.refresh()
  refreshTimer = setInterval(() => void jobStore.refresh(), 3000)
})

onUnmounted(() => {
  if (refreshTimer) clearInterval(refreshTimer)
})

type BadgeColor = 'neutral' | 'warning' | 'info' | 'success' | 'error'

function stateColor(state: JobState): BadgeColor {
  return {
    draft: 'neutral',
    waiting: 'warning',
    running: 'info',
    paused: 'warning',
    succeeded: 'success',
    failed: 'error',
    cancelled: 'neutral'
  }[state] as BadgeColor
}

function formatCreatedAt(value: string) {
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'short'
  }).format(new Date(value))
}

function canDelete(state: JobState) {
  return ['succeeded', 'failed', 'cancelled'].includes(state)
}

async function handleRemove(job: { id: string, name: string }) {
  if (!window.confirm(t('jobs.removeConfirm', { name: job.name }))) return

  deletingJobId.value = job.id
  try {
    await jobStore.remove(job.id)
    toast.add({
      title: t('jobs.removed'),
      color: 'success',
      icon: 'i-lucide-trash-2'
    })
  } catch {
    toast.add({
      title: t('jobs.removeFailed'),
      description: jobStore.lastError ?? undefined,
      color: 'error',
      icon: 'i-lucide-circle-alert'
    })
  } finally {
    deletingJobId.value = null
  }
}

async function handlePause(job: { id: string, name: string }) {
  if (!window.confirm(t('jobs.pauseRiskConfirm', { name: job.name }))) return
  try {
    await jobStore.setState(job.id, 'paused')
    toast.add({ title: t('jobs.paused'), color: 'warning', icon: 'i-lucide-pause' })
  } catch {
    toast.add({
      title: t('jobs.pauseFailed'),
      description: jobStore.lastError ?? undefined,
      color: 'error',
      icon: 'i-lucide-circle-alert'
    })
  }
}

async function handleResume(job: { id: string }) {
  try {
    await jobStore.setState(job.id, 'running')
    toast.add({ title: t('jobs.resumed'), color: 'success', icon: 'i-lucide-play' })
  } catch {
    toast.add({
      title: t('jobs.resumeFailed'),
      description: jobStore.lastError ?? undefined,
      color: 'error',
      icon: 'i-lucide-circle-alert'
    })
  }
}
</script>

<template>
  <div class="mx-auto max-w-[1500px] p-6 lg:p-8">
    <PageHeader
      :title="$t('jobs.title')"
      :description="$t('jobs.description')"
    >
      <template #actions>
        <UTooltip :text="$t('jobs.createHint')">
          <UButton
            icon="i-lucide-plus"
            :label="$t('jobs.create')"
            disabled
          />
        </UTooltip>
      </template>
    </PageHeader>

    <UAlert
      v-if="jobStore.lastError"
      class="mt-6"
      color="error"
      variant="subtle"
      icon="i-lucide-circle-alert"
      :title="$t('jobs.loadFailed')"
      :description="jobStore.lastError"
    />

    <UCard v-if="jobStore.jobs.length" class="mt-6 overflow-hidden">
      <template #header>
        <div class="flex items-center justify-between">
          <h2 class="text-base font-semibold">
            {{ $t('jobs.all') }}
          </h2>
          <UBadge color="neutral" variant="subtle">
            {{ jobStore.jobs.length }}
          </UBadge>
        </div>
      </template>

      <div class="-mx-6 -my-5 divide-y divide-default">
        <article
          v-for="job in jobStore.jobs"
          :key="job.id"
          class="grid grid-cols-[minmax(260px,1.4fr)_150px_120px_minmax(240px,1fr)_40px] items-center gap-6 px-6 py-4"
        >
          <div class="min-w-0">
            <p class="truncate text-sm font-semibold">
              {{ job.name }}
            </p>
            <p class="mt-1 text-xs text-dimmed">
              {{ formatCreatedAt(job.createdAt) }}
            </p>
          </div>
          <div>
            <p class="text-[11px] uppercase tracking-wide text-dimmed">
              {{ $t('jobs.operation') }}
            </p>
            <p class="mt-1.5 text-sm">
              {{ $t(`jobs.operations.${job.operation}`) }}
            </p>
          </div>
          <div>
            <p class="text-[11px] uppercase tracking-wide text-dimmed">
              {{ $t('common.status') }}
            </p>
            <UBadge
              class="mt-1.5"
              :color="stateColor(job.state)"
              variant="subtle"
            >
              {{ $t(`common.${job.state}`) }}
            </UBadge>
          </div>
          <div>
            <div class="flex items-center justify-between text-xs">
              <span class="text-muted">
                {{ $t('jobs.targets', { count: job.targets.length }) }}
              </span>
              <span class="font-medium">{{ job.progressPercent }}%</span>
            </div>
            <UProgress
              class="mt-2"
              :model-value="job.progressPercent"
              :color="job.state === 'failed' ? 'error' : 'primary'"
              size="sm"
            />
            <p v-if="job.statusMessage || job.stage" class="mt-2 truncate text-xs text-dimmed">
              {{ job.statusMessage || $t(`jobs.stages.${job.stage}`) }}
            </p>
            <p v-if="job.errorMessage" class="mt-2 text-xs text-error">
              {{ job.errorMessage }}
            </p>
          </div>
          <div class="flex justify-end">
            <UButton
              v-if="job.state === 'running'"
              icon="i-lucide-pause"
              color="warning"
              variant="ghost"
              size="sm"
              :aria-label="$t('jobs.pause')"
              :title="$t('jobs.pause')"
              @click="handlePause(job)"
            />
            <UButton
              v-else-if="job.state === 'paused'"
              icon="i-lucide-play"
              color="success"
              variant="ghost"
              size="sm"
              :aria-label="$t('jobs.resume')"
              :title="$t('jobs.resume')"
              @click="handleResume(job)"
            />
            <UButton
              v-else-if="canDelete(job.state)"
              icon="i-lucide-trash-2"
              color="error"
              variant="ghost"
              size="sm"
              :loading="deletingJobId === job.id"
              :disabled="deletingJobId !== null"
              :aria-label="$t('common.delete')"
              :title="$t('common.delete')"
              @click="handleRemove(job)"
            />
          </div>
        </article>
      </div>
    </UCard>

    <EmptyStatePanel
      v-else-if="!jobStore.loading"
      icon="i-lucide-list-checks"
      :title="$t('jobs.emptyTitle')"
      :description="$t('jobs.emptyDescription')"
    />

    <UCard v-else class="mt-6">
      <div class="grid min-h-80 place-items-center">
        <UIcon name="i-lucide-loader-circle" class="size-6 animate-spin text-primary" />
      </div>
    </UCard>
  </div>
</template>
