<script setup lang="ts">
import { peBrandFromName } from '~/utils/settings'

const props = defineProps<{
  name: string
}>()

const brand = computed(() => peBrandFromName(props.name))

const brandDetails = {
  easyu: { label: '优启通 EasyU', mark: 'EU', className: 'bg-transparent text-[#1677ff] ring-transparent' },
  edgeless: { label: 'Edgeless', mark: 'E', className: 'bg-transparent text-[#168fd5] ring-transparent' },
  firpe: { label: 'FirPE', mark: 'F', className: 'bg-transparent text-[#008f67] ring-transparent' },
  hotpe: { label: 'HotPE', mark: 'H', className: 'bg-transparent text-[#168fd5] ring-transparent' },
  usm: { label: 'USM U盘魔术师', mark: 'USM', className: 'bg-transparent text-[#168fd5] ring-transparent' },
  wepe: { label: '微PE WePE', mark: 'W', className: 'bg-[#21a366]/10 text-[#168a52] ring-[#21a366]/20' },
  unknown: { label: 'Windows PE', mark: '', className: 'bg-success/10 text-success ring-success/15' }
} as const

const details = computed(() => brandDetails[brand.value])
const brandLogoSrc = computed(() => {
  if (brand.value === 'easyu') return '/logos/easyu.svg'
  if (brand.value === 'edgeless') return '/logos/edgeless.svg'
  if (brand.value === 'firpe') return '/logos/firpe.svg'
  if (brand.value === 'hotpe') return '/logos/hotpe.svg'
  if (brand.value === 'usm') return '/logos/usm.svg'
  return ''
})
</script>

<template>
  <div
    class="grid size-10 shrink-0 place-items-center rounded-xl ring-1"
    :class="details.className"
    role="img"
    :aria-label="details.label"
    :title="details.label"
  >
    <img
      v-if="brandLogoSrc"
      :src="brandLogoSrc"
      alt=""
      class="size-9"
    >
    <span v-else-if="details.mark" class="text-sm font-black tracking-[-0.08em]" :class="brand === 'usm' ? 'text-[9px] tracking-tight' : ''">
      {{ details.mark }}
    </span>
    <UIcon v-else name="i-lucide-disc-3" class="size-5" />
  </div>
</template>
