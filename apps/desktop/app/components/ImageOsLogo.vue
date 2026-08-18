<script setup lang="ts">
import type { ImageOperatingSystem } from '~/utils/image-os'

const props = defineProps<{
  operatingSystem: ImageOperatingSystem
}>()

const labels: Record<ImageOperatingSystem, string> = {
  'windows-xp': 'Windows XP',
  'windows-7': 'Windows 7',
  'windows-8': 'Windows 8',
  'windows-10': 'Windows 10',
  'windows-11': 'Windows 11',
  'windows-server': 'Windows Server',
  unknown: 'Unknown operating system'
}
</script>

<template>
  <div
    class="grid size-11 shrink-0 place-items-center rounded-xl bg-primary/10 text-primary ring-1 ring-primary/15"
    role="img"
    :aria-label="labels[props.operatingSystem]"
    :title="labels[props.operatingSystem]"
  >
    <svg
      v-if="props.operatingSystem === 'windows-7' || props.operatingSystem === 'windows-xp'"
      class="size-7"
      viewBox="0 0 32 32"
      aria-hidden="true"
    >
      <path fill="#f35325" d="M3 5.5 14 4v10H3z" />
      <path fill="#81bc06" d="m16 3.7 13-1.8V14H16z" />
      <path fill="#05a6f0" d="M3 16h11v10L3 24.5z" />
      <path fill="#ffba08" d="M16 16h13v12.1l-13-1.8z" />
    </svg>

    <svg
      v-else-if="props.operatingSystem === 'windows-8' || props.operatingSystem === 'windows-10'"
      class="size-7"
      viewBox="0 0 32 32"
      aria-hidden="true"
    >
      <path fill="#00a4ef" d="m3 6.5 11-1.6v10H3zm13-1.9L29 2.8V15H16zM3 17h11v10L3 25.5zm13 0h13v12.2l-13-1.8z" />
    </svg>

    <svg
      v-else-if="props.operatingSystem === 'windows-11'"
      class="size-7"
      viewBox="0 0 32 32"
      aria-hidden="true"
    >
      <path fill="#0078d4" d="M3 3h12v12H3zm14 0h12v12H17zM3 17h12v12H3zm14 0h12v12H17z" />
    </svg>

    <div
      v-else-if="props.operatingSystem === 'windows-server'"
      class="flex flex-col items-center gap-0.5"
    >
      <svg class="size-6" viewBox="0 0 32 32" aria-hidden="true">
        <path fill="#0078d4" d="M3 4h12v11H3zm14 0h12v11H17zM3 17h12v11H3zm14 0h12v11H17z" />
      </svg>
      <span class="text-[7px] font-bold leading-none text-[#0078d4]">SERVER</span>
    </div>

    <UIcon v-else name="i-lucide-hard-drive-download" class="size-5" />
  </div>
</template>
