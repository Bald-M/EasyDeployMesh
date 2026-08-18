<script setup lang="ts">
const { initializeLocale } = useAppLocale()

useHead({
  htmlAttrs: {
    lang: 'zh-CN'
  }
})

onMounted(() => {
  initializeLocale()
})
</script>

<template>
  <UApp>
    <NuxtLayout>
      <NuxtPage />
    </NuxtLayout>
  </UApp>
</template>
