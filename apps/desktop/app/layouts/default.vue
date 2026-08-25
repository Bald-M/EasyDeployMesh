<script setup lang="ts">
import { openExternalUrl } from '~/services/external'

const route = useRoute()
const { t } = useI18n()
const { currentLocale, changeLocale } = useAppLocale()
const runtimeStore = useRuntimeStore()
const imageStore = useImageStore()
const jobStore = useJobStore()
const deviceStore = useDeviceStore()
let refreshTimer: ReturnType<typeof setInterval> | undefined
const repositoryUrl = 'https://github.com/Bald-M/EasyDeployMesh'

const globalRefreshLoading = computed(() =>
  runtimeStore.loading
  || imageStore.loading
  || jobStore.loading
  || deviceStore.loading
)

function refreshAll() {
  return Promise.all([
    runtimeStore.refresh(),
    imageStore.refresh(),
    jobStore.refresh(),
    route.path === '/devices' ? deviceStore.verifyOnline() : deviceStore.refresh()
  ])
}

const navigation = computed(() => [
  {
    label: t('nav.overview'),
    icon: 'i-lucide-layout-dashboard',
    to: '/'
  },
  {
    label: t('nav.devices'),
    icon: 'i-lucide-monitor-smartphone',
    to: '/devices'
  },
  {
    label: t('nav.images'),
    icon: 'i-lucide-layers-3',
    to: '/images'
  },
  {
    label: t('nav.jobs'),
    icon: 'i-lucide-list-checks',
    to: '/jobs'
  },
  {
    label: t('nav.logs'),
    icon: 'i-lucide-scroll-text',
    to: '/logs'
  },
  {
    label: t('nav.settings'),
    icon: 'i-lucide-settings-2',
    to: '/settings'
  }
])

const title = computed(() => {
  const key = route.meta.titleKey as string | undefined
  return key ? t(key) : t('nav.overview')
})

const languageItems = computed(() => [
  { label: '简体中文', value: 'zh-CN' },
  { label: 'English', value: 'en-US' }
])

const selectedLocale = computed({
  get: () => currentLocale.value,
  set: value => changeLocale(value)
})

onMounted(() => {
  Promise.all([
    runtimeStore.refresh(),
    imageStore.refresh(),
    jobStore.refresh(),
    deviceStore.refresh()
  ])
  refreshTimer = setInterval(() => {
    runtimeStore.refresh()
    deviceStore.refresh()
  }, 10_000)
})

onBeforeUnmount(() => {
  if (refreshTimer) {
    clearInterval(refreshTimer)
  }
})
</script>

<template>
  <div class="flex h-screen min-h-[680px] bg-default text-highlighted">
    <aside class="flex w-64 shrink-0 flex-col border-r border-default bg-elevated/75 backdrop-blur-xl">
      <div class="app-drag-region flex h-16 items-center justify-start border-b border-default px-5">
        <div class="flex max-w-full items-center gap-3">
          <div class="grid size-10 shrink-0 place-items-center">
            <img
              src="/easydeploymesh-brand-mark.svg?v=3"
              alt="EasyDeployMesh"
              class="block size-8 object-contain"
            >
          </div>
          <div class="min-w-0">
            <p class="truncate text-sm font-semibold tracking-tight">
              {{ $t('app.name') }}
            </p>
            <p class="truncate text-xs text-muted">
              {{ $t('app.tagline') }}
            </p>
          </div>
        </div>
      </div>

      <nav class="flex-1 space-y-1 p-3">
        <UButton
          v-for="item in navigation"
          :key="item.to"
          :to="item.to"
          :icon="item.icon"
          :label="item.label"
          color="neutral"
          :variant="route.path === item.to ? 'soft' : 'ghost'"
          block
          class="justify-start"
        />
      </nav>

      <div class="space-y-3 border-t border-default p-4">
        <div class="rounded-xl border border-default bg-default/70 p-3">
          <div class="flex items-center justify-between gap-3">
            <div class="flex min-w-0 items-center gap-2">
              <span class="relative flex size-2">
                <span class="absolute inline-flex size-full animate-ping rounded-full bg-amber-400 opacity-50" />
                <span class="relative inline-flex size-2 rounded-full bg-amber-500" />
              </span>
              <span class="truncate text-xs font-medium">
                {{ $t('dashboard.service') }}
              </span>
            </div>
            <UBadge color="neutral" variant="subtle" size="sm">
              {{ $t(`common.${runtimeStore.status.serviceState}`) }}
            </UBadge>
          </div>
          <p class="mt-2 truncate text-xs text-muted">
            v{{ runtimeStore.status.version }} · {{ runtimeStore.status.platform }}
          </p>
        </div>
      </div>
    </aside>

    <div class="flex min-w-0 flex-1 flex-col">
      <header class="app-drag-region flex h-16 shrink-0 items-center justify-between border-b border-default bg-default/80 px-6 backdrop-blur-xl">
        <div>
          <p class="text-sm font-semibold">
            {{ title }}
          </p>
          <p class="text-xs text-muted">
            EasyDeployMesh v{{ runtimeStore.status.version }}
          </p>
        </div>

        <div class="app-no-drag flex items-center gap-2">
          <USelect
            v-model="selectedLocale"
            :items="languageItems"
            value-key="value"
            class="w-36"
            icon="i-lucide-languages"
            aria-label="Language"
          />
          <UColorModeButton color="neutral" variant="ghost" />
          <UButton
            icon="i-simple-icons-github"
            color="neutral"
            variant="ghost"
            aria-label="GitHub"
            @click="openExternalUrl(repositoryUrl)"
          />
          <UButton
            icon="i-lucide-refresh-cw"
            color="neutral"
            variant="ghost"
            :loading="globalRefreshLoading"
            :aria-label="$t('common.refresh')"
            @click="refreshAll"
          />
        </div>
      </header>

      <main class="min-h-0 flex-1 overflow-y-auto">
        <slot />
      </main>
    </div>
  </div>
</template>
