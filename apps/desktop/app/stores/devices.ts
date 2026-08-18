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

  function refresh() {
    if (refreshInFlight) return refreshInFlight
    refreshInFlight = (async () => {
      activeRequests.value += 1
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
        activeRequests.value -= 1
        refreshInFlight = null
      }
    })()
    return refreshInFlight
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
