<script setup lang="ts">
import { classifyImageDeploymentSupport } from '~/utils/image-deployment-support'

definePageMeta({
  titleKey: 'nav.overview'
})

const runtimeStore = useRuntimeStore()
const imageStore = useImageStore()
const jobStore = useJobStore()
const { t } = useI18n()

const deployableVerifiedCount = computed(() => imageStore.images.filter(image => (
  ['automatic', 'manual'].includes(classifyImageDeploymentSupport(image.format, image.verified))
)).length)

const metrics = computed(() => [
  {
    label: t('dashboard.service'),
    value: t(`common.${runtimeStore.status.serviceState}`),
    hint: t('dashboard.serviceHint'),
    icon: 'i-lucide-radio-tower'
  },
  {
    label: t('dashboard.devices'),
    value: runtimeStore.status.connectedDevices,
    hint: t('dashboard.devicesHint'),
    icon: 'i-lucide-monitor-check'
  },
  {
    label: t('dashboard.images'),
    value: deployableVerifiedCount.value,
    hint: t('dashboard.imagesHint'),
    icon: 'i-lucide-layers-3'
  },
  {
    label: t('dashboard.queued'),
    value: jobStore.queuedCount,
    hint: t('dashboard.queuedHint'),
    icon: 'i-lucide-clock-3'
  }
])

const setupSteps = computed(() => [
  {
    title: t('dashboard.networkStep'),
    description: t('dashboard.networkStepHint'),
    icon: 'i-lucide-cable',
    complete: runtimeStore.status.activeInterface !== null,
    to: '/settings#network'
  },
  {
    title: t('dashboard.bootPackStep'),
    description: t('dashboard.bootPackStepHint'),
    icon: 'i-lucide-package-open',
    complete: false,
    to: '/settings#boot-pack'
  },
  {
    title: t('dashboard.imageStep'),
    description: t('dashboard.imageStepHint'),
    icon: 'i-lucide-hard-drive-download',
    complete: deployableVerifiedCount.value > 0,
    to: '/images'
  }
])

const recentJobs = computed(() => jobStore.jobs.slice(0, 4))
</script>

<template>
  <div class="mx-auto max-w-[1500px] space-y-6 p-6 lg:p-8">
    <section class="relative overflow-hidden rounded-2xl border border-primary/20 bg-elevated px-7 py-8 shadow-sm">
      <div class="pointer-events-none absolute inset-y-0 right-0 w-2/5 bg-gradient-to-l from-primary/10 to-transparent" />
      <div class="relative max-w-3xl">
        <div class="mb-4 flex items-center gap-2">
          <span class="h-px w-8 bg-primary" />
          <span class="text-xs font-semibold uppercase tracking-[0.18em] text-primary">
            {{ $t('dashboard.eyebrow') }}
          </span>
        </div>
        <h1 class="text-3xl font-semibold tracking-tight text-highlighted">
          {{ $t('dashboard.title') }}
        </h1>
        <p class="mt-3 max-w-2xl text-sm leading-6 text-muted">
          {{ $t('dashboard.description') }}
        </p>
        <div class="mt-6 flex items-center gap-3">
          <UButton
            to="/settings"
            icon="i-lucide-sliders-horizontal"
            :label="$t('dashboard.startSetup')"
          />
          <UButton
            to="/devices"
            icon="i-lucide-monitor-smartphone"
            color="neutral"
            variant="outline"
            :label="$t('dashboard.viewDevices')"
          />
        </div>
      </div>
    </section>

    <UAlert
      v-if="runtimeStore.errorCode"
      color="error"
      variant="subtle"
      icon="i-lucide-circle-alert"
      :title="$t(runtimeStore.errorCode)"
    />

    <section class="grid grid-cols-4 gap-4">
      <MetricCard
        v-for="metric in metrics"
        :key="metric.label"
        v-bind="metric"
      />
    </section>

    <section class="grid grid-cols-5 gap-6">
      <UCard class="col-span-3">
        <template #header>
          <div>
            <h2 class="text-base font-semibold text-highlighted">
              {{ $t('dashboard.setupTitle') }}
            </h2>
            <p class="mt-1 text-sm text-muted">
              {{ $t('dashboard.setupDescription') }}
            </p>
          </div>
        </template>

        <div class="space-y-3">
          <NuxtLink
            v-for="(step, index) in setupSteps"
            :key="step.title"
            :to="step.to"
            class="group flex min-h-24 items-center gap-5 rounded-xl border border-transparent px-4 py-5 transition-colors hover:border-default hover:bg-elevated focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/50"
          >
            <div
              class="grid size-11 shrink-0 place-items-center rounded-xl ring-1"
              :class="step.complete
                ? 'bg-success/10 text-success ring-success/20'
                : 'bg-elevated text-muted ring-default'"
            >
              <UIcon
                :name="step.complete ? 'i-lucide-check' : step.icon"
                class="size-5"
              />
            </div>
            <div class="min-w-0 flex-1 space-y-2">
              <div class="flex items-center gap-2.5">
                <span class="text-xs font-medium text-dimmed">
                  0{{ index + 1 }}
                </span>
                <h3 class="text-sm font-semibold text-highlighted">
                  {{ step.title }}
                </h3>
              </div>
              <p class="text-sm leading-6 text-muted">
                {{ step.description }}
              </p>
            </div>
            <UIcon name="i-lucide-chevron-right" class="size-4 shrink-0 text-dimmed transition-transform group-hover:translate-x-0.5 group-hover:text-primary" />
          </NuxtLink>
        </div>
      </UCard>

      <UCard class="col-span-2">
        <template #header>
          <div class="flex items-center justify-between gap-4">
            <div>
              <h2 class="text-base font-semibold text-highlighted">
                {{ $t('dashboard.networkTitle') }}
              </h2>
              <p class="mt-1 text-sm text-muted">
                {{ $t('dashboard.networkDescription') }}
              </p>
            </div>
            <UBadge color="neutral" variant="subtle">
              {{ runtimeStore.usableInterfaces.length }}
            </UBadge>
          </div>
        </template>

        <div v-if="runtimeStore.usableInterfaces.length" class="space-y-2">
          <div
            v-for="networkInterface in runtimeStore.usableInterfaces"
            :key="`${networkInterface.name}-${networkInterface.address}`"
            class="flex items-center gap-3 rounded-xl border border-default bg-elevated/50 p-3"
          >
            <div class="grid size-9 place-items-center rounded-lg bg-primary/10 text-primary">
              <UIcon name="i-lucide-ethernet-port" class="size-4" />
            </div>
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm font-medium">
                {{ networkInterface.name }}
              </p>
              <p class="truncate font-mono text-xs text-muted">
                {{ networkInterface.address }}
              </p>
            </div>
            <UBadge color="neutral" variant="outline" size="sm">
              IPv{{ networkInterface.address.includes(':') ? '6' : '4' }}
            </UBadge>
          </div>
        </div>

        <div v-else class="grid min-h-44 place-items-center text-center">
          <div>
            <UIcon name="i-lucide-unplug" class="mx-auto size-6 text-dimmed" />
            <p class="mt-3 text-sm text-muted">
              {{ $t('common.notConfigured') }}
            </p>
          </div>
        </div>
      </UCard>
    </section>

    <UCard>
      <template #header>
        <div>
          <h2 class="text-base font-semibold text-highlighted">
            {{ $t('dashboard.recentTitle') }}
          </h2>
          <p class="mt-1 text-sm text-muted">
            {{ $t('dashboard.recentDescription') }}
          </p>
        </div>
      </template>

      <div v-if="recentJobs.length" class="divide-y divide-default">
        <div
          v-for="job in recentJobs"
          :key="job.id"
          class="flex items-center gap-4 py-3 first:pt-0 last:pb-0"
        >
          <div class="grid size-9 place-items-center rounded-lg bg-primary/10 text-primary">
            <UIcon name="i-lucide-monitor-down" class="size-4" />
          </div>
          <div class="min-w-0 flex-1">
            <p class="truncate text-sm font-medium">
              {{ job.name }}
            </p>
            <p class="mt-0.5 text-xs text-muted">
              {{ $t(`jobs.operations.${job.operation}`) }} ·
              {{ $t('jobs.targets', { count: job.targets.length }) }}
            </p>
          </div>
          <UBadge
            :color="job.state === 'failed'
              ? 'error'
              : job.state === 'succeeded'
                ? 'success'
                : 'neutral'"
            variant="subtle"
          >
            {{ $t(`common.${job.state}`) }}
          </UBadge>
        </div>
      </div>

      <div v-else class="grid min-h-36 place-items-center text-center">
        <div>
          <UIcon name="i-lucide-list-checks" class="mx-auto size-6 text-dimmed" />
          <p class="mt-3 text-sm font-medium">
            {{ $t('dashboard.noJobs') }}
          </p>
          <p class="mt-1 text-xs text-muted">
            {{ $t('dashboard.noJobsHint') }}
          </p>
        </div>
      </div>
    </UCard>
  </div>
</template>
