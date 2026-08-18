import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

describe('dashboard setup navigation', () => {
  it('links every setup step to its configuration destination', () => {
    const dashboard = readFileSync(resolve('app/pages/index.vue'), 'utf8')
    const settings = readFileSync(resolve('app/pages/settings.vue'), 'utf8')

    expect(dashboard).toContain("to: '/settings#network'")
    expect(dashboard).toContain("to: '/settings#boot-pack'")
    expect(dashboard).toContain("to: '/images'")
    expect(dashboard).toContain('<NuxtLink')
    expect(settings).toContain('id="network"')
    expect(settings).toContain('id="boot-pack"')
  })
})
