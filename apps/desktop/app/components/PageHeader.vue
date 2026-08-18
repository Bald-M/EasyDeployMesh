<script setup lang="ts">
defineProps<{
  title: string
  description: string
}>()
</script>

<template>
  <div class="flex items-start justify-between gap-6">
    <div class="max-w-3xl">
      <h1 class="text-2xl font-semibold tracking-tight text-highlighted">
        {{ title }}
      </h1>
      <p class="mt-2 text-sm leading-6 text-muted">
        {{ description }}
      </p>
    </div>
    <div class="flex shrink-0 items-center gap-2">
      <slot name="actions" />
    </div>
  </div>
</template>

