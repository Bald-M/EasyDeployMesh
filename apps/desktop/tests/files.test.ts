import { describe, expect, it } from 'vitest'
import { compactHash, formatBytes } from '../app/utils/files'

describe('deployment file formatting', () => {
  it('uses binary units for image sizes', () => {
    expect(formatBytes(0, 'en-US')).toBe('0 B')
    expect(formatBytes(1536, 'en-US')).toBe('1.5 KB')
    expect(formatBytes(5 * 1024 ** 3, 'en-US')).toBe('5 GB')
  })

  it('compacts a SHA-256 digest without losing both ends', () => {
    const hash = '1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef'
    expect(compactHash(hash)).toBe('12345678…90abcdef')
    expect(compactHash(null)).toBe('—')
  })
})
