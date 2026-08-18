<script setup lang="ts">
withDefaults(defineProps<{
  icon: string
  title: string
  description: string
}>(), {
  icon: 'i-lucide-inbox'
})
</script>

<template>
  <UCard class="mt-6">
    <div class="grid min-h-80 place-items-center px-6 py-14 text-center">
      <div>
        <div class="mx-auto grid size-14 place-items-center rounded-2xl bg-elevated ring-1 ring-default">
          <UIcon :name="icon" class="size-6 text-muted" />
        </div>
        <h2 class="mt-5 text-base font-semibold text-highlighted">
          {{ title }}
        </h2>
        <p class="mx-auto mt-2 max-w-md text-sm leading-6 text-muted">
          {{ description }}
        </p>
        <div class="mt-6">
          <slot />
        </div>
      </div>
    </div>
  </UCard>
</template>

