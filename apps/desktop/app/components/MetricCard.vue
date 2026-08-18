<script setup lang="ts">
defineProps<{
  label: string
  value: string | number
  hint: string
  icon: string
  color?: 'primary' | 'success' | 'warning' | 'neutral'
}>()
</script>

<template>
  <UCard>
    <div class="flex items-start justify-between gap-4">
      <div class="min-w-0">
        <p class="text-sm font-medium text-muted">
          {{ label }}
        </p>
        <p class="mt-3 text-3xl font-semibold tracking-tight text-highlighted">
          {{ value }}
        </p>
        <p class="mt-2 truncate text-xs text-dimmed">
          {{ hint }}
        </p>
      </div>
      <div class="grid size-10 shrink-0 place-items-center rounded-xl bg-primary/10 text-primary">
        <UIcon :name="icon" class="size-5" />
      </div>
    </div>
  </UCard>
</template>

