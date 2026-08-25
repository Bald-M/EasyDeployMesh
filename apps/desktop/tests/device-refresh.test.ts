import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

describe('manual device refresh', () => {
  it('uses the active heartbeat verification command instead of only listing cached state', () => {
    const service = readFileSync(resolve('app/services/deployment.ts'), 'utf8')
    const layout = readFileSync(resolve('app/layouts/default.vue'), 'utf8')
    const devicesPage = readFileSync(resolve('app/pages/devices.vue'), 'utf8')

    expect(service).toContain("verifyOnline ? 'refresh_devices' : 'list_devices'")
    expect(layout).toContain("route.path === '/devices' ? deviceStore.verifyOnline() : deviceStore.refresh()")
    expect(layout).toContain('@click="refreshAll"')
    expect(devicesPage).toContain('jobStore.refresh()')
    expect(devicesPage).toContain('refreshTimer = setInterval(() => void refreshClients(), 5_000)')
  })
})
