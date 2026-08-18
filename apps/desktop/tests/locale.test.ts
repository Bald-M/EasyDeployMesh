import { describe, expect, it } from 'vitest'
import { isAppLocale, resolveLocale } from '../app/utils/locale'

describe('locale resolution', () => {
  it('maps Chinese system locales to simplified Chinese', () => {
    expect(resolveLocale('zh-CN')).toBe('zh-CN')
    expect(resolveLocale('zh-Hans-NZ')).toBe('zh-CN')
  })

  it('uses English for supported and unknown non-Chinese locales', () => {
    expect(resolveLocale('en-NZ')).toBe('en-US')
    expect(resolveLocale('ja-JP')).toBe('en-US')
    expect(resolveLocale(undefined)).toBe('en-US')
  })

  it('accepts only application locale identifiers', () => {
    expect(isAppLocale('zh-CN')).toBe(true)
    expect(isAppLocale('en-US')).toBe(true)
    expect(isAppLocale('en-NZ')).toBe(false)
  })
})

