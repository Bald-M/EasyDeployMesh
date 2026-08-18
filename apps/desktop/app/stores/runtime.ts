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

const initialStatus: RuntimeStatus = {
  serviceState: 'idle',
  version: '0.2.4',
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

  async function refresh() {
    refreshing.value = true
    errorCode.value = null

    try {
      const [nextStatus, nextInterfaces, nextControlStatus, nextPxeStatus] = await Promise.all([
        getRuntimeStatus(),
        getNetworkInterfaces(),
        getControlPlaneStatus(),
        getPxeServiceStatus()
      ])
      status.value = nextStatus
      interfaces.value = nextInterfaces
      controlStatus.value = nextControlStatus
      pxeStatus.value = nextPxeStatus
    } catch {
      errorCode.value = 'errors.runtimeUnavailable'
    } finally {
      refreshing.value = false
    }
  }

  async function start(bindAddress: string, port = 7760) {
    controlLoading.value = true
    errorCode.value = null
    try {
      controlStatus.value = await startControlPlane(bindAddress, port)
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
      controlStatus.value = await stopControlPlane()
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
      pxeStatus.value = await startPxeService(config, controlPort)
      controlStatus.value = await getControlPlaneStatus()
    }
    finally { pxeLoading.value = false }
  }

  async function stopPxe() {
    pxeLoading.value = true
    try { pxeStatus.value = await stopPxeService() }
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
