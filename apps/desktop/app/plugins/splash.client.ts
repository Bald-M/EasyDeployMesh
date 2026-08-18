const MINIMUM_SPLASH_DURATION_MS = 900

export default defineNuxtPlugin((nuxtApp) => {
  const completeHydration = nuxtApp.deferHydration()
  const remainingDuration = Math.max(
    0,
    MINIMUM_SPLASH_DURATION_MS - performance.now()
  )

  window.setTimeout(completeHydration, remainingDuration)
})
