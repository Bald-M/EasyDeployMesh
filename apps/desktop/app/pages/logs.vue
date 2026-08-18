<script setup lang="ts">
import { getActivityEvents } from '~/services/runtime'
import type { ActivityEvent, ActivitySeverity, ActivitySource } from '~/types/runtime'
import { groupActivityEvents, severitiesForStatus, sourcesForFilter, type ActivityStatusFilter } from '~/utils/activity-events'

definePageMeta({ titleKey: 'nav.logs' })

const { locale, t } = useI18n()
const events = ref<ActivityEvent[]>([])
const statusFilter = ref<ActivityStatusFilter>('all')
const sourceFilter = ref<ActivitySource | 'all'>('all')
const expanded = ref(new Set<string>())
const loading = ref(false)
const loadingMore = ref(false)
const loadError = ref(false)
const hasMore = ref(false)
let timer: ReturnType<typeof setInterval> | undefined

const groupedEvents = computed(() => groupActivityEvents(events.value))
const filtersActive = computed(() => statusFilter.value !== 'all' || sourceFilter.value !== 'all')
const statusItems = computed(() => [
  { value: 'all' as const, label: t('logs.filters.all') },
  { value: 'normal' as const, label: t('logs.filters.normal') },
  { value: 'abnormal' as const, label: t('logs.filters.abnormal') }
])
const sourceItems = computed(() => [
  { value: 'all' as const, label: t('logs.sources.all') },
  { value: 'service' as const, label: t('logs.sources.service') },
  { value: 'device' as const, label: t('logs.sources.device') },
  { value: 'deployment' as const, label: t('logs.sources.deployment') }
])

function query(extra: { before?: string, after?: string } = {}) {
  return {
    sources: sourcesForFilter(sourceFilter.value),
    severities: severitiesForStatus(statusFilter.value),
    limit: 200,
    ...extra
  }
}

async function reload() {
  loading.value = true
  loadError.value = false
  try {
    const next = await getActivityEvents(query())
    events.value = next
    hasMore.value = next.length === 200
    expanded.value = new Set()
  } catch {
    loadError.value = true
  } finally {
    loading.value = false
  }
}

async function refreshNew() {
  if (!events.value.length) return reload()
  try {
    const next = await getActivityEvents(query({ after: events.value[0]?.occurredAt }))
    if (!next.length) return
    const ids = new Set(events.value.map(event => event.id))
    events.value = [...next.filter(event => !ids.has(event.id)), ...events.value]
      .sort((left, right) => right.occurredAt.localeCompare(left.occurredAt))
  } catch {
    loadError.value = true
  }
}

async function loadMore() {
  const oldest = events.value.at(-1)
  if (!oldest || loadingMore.value) return
  loadingMore.value = true
  try {
    const next = await getActivityEvents(query({ before: oldest.occurredAt }))
    const ids = new Set(events.value.map(event => event.id))
    events.value.push(...next.filter(event => !ids.has(event.id)))
    hasMore.value = next.length === 200
  } finally {
    loadingMore.value = false
  }
}

function clearFilters() {
  statusFilter.value = 'all'
  sourceFilter.value = 'all'
}

function toggle(id: string) {
  const next = new Set(expanded.value)
  next.has(id) ? next.delete(id) : next.add(id)
  expanded.value = next
}

const summaries: Record<string, string> = {
  control_service_started: 'controlStarted', control_service_stopped: 'controlStopped', control_service_failed: 'controlFailed',
  pxe_service_started: 'pxeStarted', pxe_service_stopped: 'pxeStopped', pxe_service_failed: 'pxeFailed',
  pxe_request_accepted: 'pxeAccepted', boot_file_sent: 'bootFileSent', tftp_failed: 'tftpFailed',
  device_registered: 'deviceRegistered', device_reconnected: 'deviceReconnected', device_offline: 'deviceOffline',
  job_created: 'jobCreated', job_queued: 'jobQueued', job_started: 'jobStarted', job_stage_changed: 'jobStageChanged',
  job_paused: 'jobPaused', job_resumed: 'jobResumed', job_succeeded: 'jobSucceeded', job_failed: 'jobFailed', job_cancelled: 'jobCancelled'
}

function summary(event: ActivityEvent) {
  const key = summaries[event.kind]
  if (!key) return t('logs.events.generic')
  if (event.kind === 'job_stage_changed' && typeof event.details.stage === 'string') {
    return t(`logs.events.${key}`, { stage: t(`jobs.stages.${event.details.stage}`) })
  }
  return t(`logs.events.${key}`, event.details)
}

function shortTime(value: string) {
  return new Intl.DateTimeFormat(locale.value, { hour: '2-digit', minute: '2-digit', second: '2-digit' }).format(new Date(value))
}

function fullTime(value: string) {
  return new Intl.DateTimeFormat(locale.value, { dateStyle: 'medium', timeStyle: 'medium' }).format(new Date(value))
}

function severityColor(severity: ActivitySeverity) {
  return ({ info: 'text-info', success: 'text-success', warning: 'text-warning', error: 'text-error' } as const)[severity]
}

function eventIcon(event: ActivityEvent) {
  if (event.severity === 'error') return 'i-lucide-circle-x'
  if (event.severity === 'warning') return 'i-lucide-triangle-alert'
  if (event.severity === 'success') return 'i-lucide-circle-check'
  return event.source === 'deployment' ? 'i-lucide-monitor-cog' : event.source === 'device' ? 'i-lucide-monitor' : 'i-lucide-server'
}

function detailEntries(event: ActivityEvent) {
  const entries: Array<[string, string]> = [
    [t('logs.details.time'), fullTime(event.occurredAt)],
    [t('logs.details.source'), t(`logs.sources.${event.source}`)],
    [t('logs.details.severity'), t(`logs.severities.${event.severity}`)]
  ]
  if (event.subject?.id) entries.push([t('logs.details.subjectId'), event.subject.id])
  for (const [key, value] of Object.entries(event.details)) {
    if (value !== null && value !== undefined && value !== '') entries.push([t(`logs.detailFields.${key}`), String(value)])
  }
  if (event.rawMessage) entries.push([t('logs.details.rawError'), event.rawMessage])
  return entries
}

watch([statusFilter, sourceFilter], () => void reload())
onMounted(() => { void reload(); timer = setInterval(() => void refreshNew(), 5_000) })
onBeforeUnmount(() => { if (timer) clearInterval(timer) })
</script>

<template>
  <div class="mx-auto max-w-[1200px] p-6 lg:p-8">
    <PageHeader :title="$t('logs.title')" :description="$t('logs.description')">
      <template #actions>
        <UButton icon="i-lucide-file-archive" color="neutral" variant="outline" :label="$t('logs.export')" disabled />
      </template>
    </PageHeader>

    <UCard class="mt-6">
      <div class="flex flex-col gap-4 lg:flex-row lg:items-center lg:justify-between">
        <div class="flex flex-wrap items-center gap-2">
          <div class="flex rounded-lg bg-elevated p-1">
            <UButton v-for="item in statusItems" :key="item.value" size="sm" :variant="statusFilter === item.value ? 'solid' : 'ghost'" :color="statusFilter === item.value ? 'primary' : 'neutral'" @click="statusFilter = item.value">{{ item.label }}</UButton>
          </div>
          <USelect v-model="sourceFilter" :items="sourceItems" value-key="value" class="w-44" />
          <UButton v-if="filtersActive" size="sm" variant="ghost" color="neutral" icon="i-lucide-filter-x" :label="$t('logs.clearFilters')" @click="clearFilters" />
        </div>
        <p class="text-xs text-muted">{{ $t('logs.visibleCount', { count: groupedEvents.length }) }}</p>
      </div>
    </UCard>

    <UAlert v-if="loadError" class="mt-4" color="error" variant="subtle" icon="i-lucide-circle-alert" :title="$t('logs.loadFailed')" />

    <div v-if="groupedEvents.length" class="mt-4 overflow-hidden rounded-xl border border-default bg-default">
      <article v-for="event in groupedEvents" :key="event.id" class="border-b border-default last:border-b-0">
        <button class="grid w-full grid-cols-[36px_minmax(0,1fr)_auto] items-start gap-3 px-4 py-4 text-left hover:bg-elevated/60 sm:grid-cols-[36px_minmax(0,1fr)_auto_24px] sm:px-5" type="button" @click="toggle(event.id)">
          <span class="mt-0.5 flex size-8 items-center justify-center rounded-full bg-elevated">
            <UIcon :name="eventIcon(event)" class="size-4" :class="severityColor(event.severity)" />
          </span>
          <span class="min-w-0">
            <span class="block text-sm font-medium">{{ summary(event) }}</span>
            <span class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">
              <span v-if="event.subject?.name">{{ event.subject.name }}</span>
              <span v-if="event.subject?.name">·</span>
              <span>{{ $t(`logs.sources.${event.source}`) }}</span>
              <UBadge v-if="event.count > 1" size="xs" color="neutral" variant="subtle">{{ $t('logs.repeated', { count: event.count }) }}</UBadge>
            </span>
          </span>
          <time class="whitespace-nowrap pt-0.5 text-xs text-dimmed">{{ shortTime(event.lastOccurredAt) }}</time>
          <UIcon :name="expanded.has(event.id) ? 'i-lucide-chevron-up' : 'i-lucide-chevron-down'" class="hidden size-4 text-dimmed sm:block" />
        </button>
        <div v-if="expanded.has(event.id)" class="border-t border-default bg-elevated/35 px-5 py-4 sm:pl-16">
          <dl class="grid gap-x-8 gap-y-3 text-xs sm:grid-cols-2">
            <div v-for="([label, value], index) in detailEntries(event)" :key="`${label}-${index}`" class="min-w-0">
              <dt class="text-dimmed">{{ label }}</dt>
              <dd class="mt-1 break-all font-mono text-muted">{{ value }}</dd>
            </div>
            <div v-if="event.count > 1">
              <dt class="text-dimmed">{{ $t('logs.details.period') }}</dt>
              <dd class="mt-1 text-muted">{{ fullTime(event.firstOccurredAt) }} – {{ fullTime(event.lastOccurredAt) }}</dd>
            </div>
          </dl>
        </div>
      </article>
    </div>

    <div v-if="hasMore" class="mt-4 text-center">
      <UButton color="neutral" variant="outline" :loading="loadingMore" :label="$t('logs.loadMore')" @click="loadMore" />
    </div>

    <EmptyStatePanel v-else-if="!loading && !groupedEvents.length" icon="i-lucide-scroll-text" :title="filtersActive ? $t('logs.noMatchesTitle') : $t('logs.emptyTitle')" :description="filtersActive ? $t('logs.noMatchesDescription') : $t('logs.emptyDescription')" />
  </div>
</template>
