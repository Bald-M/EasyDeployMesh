import { defineStore } from 'pinia'
import { getDevices, removeDevice } from '~/services/deployment'
import { getPxeDiscoveredClients } from '~/services/runtime'
import type { RegisteredDevice } from '~/types/deployment'
import type { PxeDiscoveredClient } from '~/types/runtime'

export const useDeviceStore = defineStore('devices', () => {
  const devices = ref<RegisteredDevice[]>([])
  const pxeClients = ref<PxeDiscoveredClient[]>([])
  const activeRequests = ref(0)
  const loading = computed(() => activeRequests.value > 0)
  const lastError = ref<string | null>(null)
  let refreshInFlight: Promise<void> | null = null
  let verificationInFlight: Promise<void> | null = null
  let requestSequence = 0

  const onlineDevices = computed(() =>
    devices.value.filter(device => device.online)
  )
  const pendingPxeClients = computed(() => {
    const registered = new Set(devices.value.map(entry => entry.device.macAddress.toUpperCase()))
    return pxeClients.value.filter(entry => !registered.has(entry.macAddress.toUpperCase()))
  })

  // Background refreshes (polling) never toggle the user-visible `loading`.
  function refresh(options: { background?: boolean } = {}) {
    const visible = !options.background
    if (refreshInFlight) {
      if (!visible) return refreshInFlight
      // A user-visible call joining a background poll still shows the spinner.
      activeRequests.value += 1
      return refreshInFlight.finally(() => { activeRequests.value -= 1 })
    }
    let request: Promise<void> | undefined
    request = (async () => {
      if (visible) activeRequests.value += 1
      lastError.value = null
      try {
        const sequence = ++requestSequence
        const [registered, discovered] = await Promise.all([getDevices(), getPxeDiscoveredClients()])
        if (sequence === requestSequence) {
          devices.value = registered
          pxeClients.value = discovered
        }
      } catch (error) {
        lastError.value = String(error)
      } finally {
        if (visible) activeRequests.value -= 1
        if (refreshInFlight === request) refreshInFlight = null
      }
    })()
    refreshInFlight = request
    return request
  }

  function verifyOnline() {
    if (verificationInFlight) return verificationInFlight
    verificationInFlight = (async () => {
      const sequence = ++requestSequence
      activeRequests.value += 1
      lastError.value = null
      try {
        const [registered, discovered] = await Promise.all([getDevices(true), getPxeDiscoveredClients()])
        if (sequence === requestSequence) {
          devices.value = registered
          pxeClients.value = discovered
        }
      } catch (error) {
        lastError.value = String(error)
      } finally {
        activeRequests.value -= 1
        verificationInFlight = null
      }
    })()
    return verificationInFlight
  }

  async function remove(id: string) {
    lastError.value = null
    try {
      const removed = await removeDevice(id)
      requestSequence += 1
      refreshInFlight = null
      if (removed) {
        devices.value = devices.value.filter(entry => entry.device.id !== id)
      }
      return removed
    } catch (error) {
      lastError.value = String(error)
      throw error
    }
  }

  return {
    devices,
    pxeClients,
    pendingPxeClients,
    onlineDevices,
    loading,
    lastError,
    refresh,
    verifyOnline,
    remove
  }
})
