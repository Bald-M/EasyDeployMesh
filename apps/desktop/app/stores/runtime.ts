import { defineStore } from 'pinia'
import {
  getControlPlaneStatus,
  getPxeServiceStatus,
  getNetworkInterfaces,
  getRuntimeStatus,
  startControlPlane,
  stopControlPlane,
  startPxeService,
  stopPxeService
} from '~/services/runtime'
import type {
  ControlPlaneStatus,
  NetworkInterfaceSummary,
  PxeConfig,
  PxeServiceStatus,
  RuntimeStatus
} from '~/types/runtime'
import { createLatestRefresh } from '~/utils/refresh-coordination'

const initialStatus: RuntimeStatus = {
  serviceState: 'idle',
  version: '0.2.6',
  platform: 'unknown',
  activeInterface: null,
  connectedDevices: 0,
  queuedJobs: 0
}

const initialControlStatus: ControlPlaneStatus = {
  state: 'idle',
  bindAddress: null,
  port: null,
  endpoint: null,
  enrollmentToken: null
}

const initialPxeStatus: PxeServiceStatus = {
  state: 'idle', mode: null, bindAddress: null, dhcpPort: null,
  proxyDhcpPort: null, tftpPort: null, activeLeases: 0, lastError: null
}

export const useRuntimeStore = defineStore('runtime', () => {
  const status = ref<RuntimeStatus>(initialStatus)
  const controlStatus = ref<ControlPlaneStatus>(initialControlStatus)
  const pxeStatus = ref<PxeServiceStatus>(initialPxeStatus)
  const interfaces = ref<NetworkInterfaceSummary[]>([])
  const refreshing = ref(false)
  const controlLoading = ref(false)
  const pxeLoading = ref(false)
  const loading = computed(() => refreshing.value || controlLoading.value || pxeLoading.value)
  const errorCode = ref<string | null>(null)

  const usableInterfaces = computed(() =>
    interfaces.value.filter(item => !item.isLoopback)
  )

  const latest = createLatestRefresh(
    () => Promise.all([
      getRuntimeStatus(),
      getNetworkInterfaces(),
      getControlPlaneStatus(),
      getPxeServiceStatus()
    ]),
    ([nextStatus, nextInterfaces, nextControlStatus, nextPxeStatus]) => {
      status.value = nextStatus
      interfaces.value = nextInterfaces
      controlStatus.value = nextControlStatus
      pxeStatus.value = nextPxeStatus
    }
  )

  async function refresh(options: { background?: boolean } = {}) {
    if (!options.background) refreshing.value = true
    errorCode.value = null

    try {
      await latest.run()
    } catch {
      errorCode.value = 'errors.runtimeUnavailable'
    } finally {
      if (!options.background) refreshing.value = false
    }
  }

  async function start(bindAddress: string, port = 7760) {
    controlLoading.value = true
    errorCode.value = null
    try {
      const next = await startControlPlane(bindAddress, port)
      latest.invalidate()
      controlStatus.value = next
      await refresh()
    } catch (error) {
      errorCode.value = 'errors.controlStartFailed'
      throw error
    } finally {
      controlLoading.value = false
    }
  }

  async function stop() {
    controlLoading.value = true
    errorCode.value = null
    try {
      const next = await stopControlPlane()
      latest.invalidate()
      controlStatus.value = next
      await refresh()
    } catch (error) {
      errorCode.value = 'errors.controlStopFailed'
      throw error
    } finally {
      controlLoading.value = false
    }
  }

  async function startPxe(config: PxeConfig, controlPort = 7760) {
    pxeLoading.value = true
    try {
      const next = await startPxeService(config, controlPort)
      latest.invalidate()
      pxeStatus.value = next
      controlStatus.value = await getControlPlaneStatus()
    }
    finally { pxeLoading.value = false }
  }

  async function stopPxe() {
    pxeLoading.value = true
    try {
      const next = await stopPxeService()
      latest.invalidate()
      pxeStatus.value = next
    }
    finally { pxeLoading.value = false }
  }

  return {
    status,
    controlStatus,
    pxeStatus,
    interfaces,
    usableInterfaces,
    loading,
    refreshing,
    controlLoading,
    pxeLoading,
    errorCode,
    refresh,
    start,
    stop,
    startPxe,
    stopPxe
  }
})
