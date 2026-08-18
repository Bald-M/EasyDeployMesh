import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

describe('desktop splash screen', () => {
  it('covers the native WebView pre-paint and remains visible during startup', () => {
    const tauriConfig = JSON.parse(readFileSync(
      resolve('src-tauri/tauri.conf.json'),
      'utf8'
    ))
    const splashPlugin = readFileSync(
      resolve('app/plugins/splash.client.ts'),
      'utf8'
    )

    expect(tauriConfig.app.windows[0].backgroundColor).toBe('#020617')
    expect(splashPlugin).toContain('deferHydration()')
    expect(splashPlugin).toContain('MINIMUM_SPLASH_DURATION_MS')
  })
})
