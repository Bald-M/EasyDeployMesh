import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

describe('browser deployment fallback', () => {
  it('guards image imports and job creation behind the native runtime', () => {
    const service = readFileSync(resolve('app/services/deployment.ts'), 'utf8')

    expect(service).toMatch(/function requireNativeMutation\(\)/u)
    expect(service).toMatch(/function importImage[\s\S]*?requireNativeMutation\(\)[\s\S]*?import_image/u)
    expect(service).toMatch(/function createJob[\s\S]*?requireNativeMutation\(\)[\s\S]*?create_job/u)
  })
})
