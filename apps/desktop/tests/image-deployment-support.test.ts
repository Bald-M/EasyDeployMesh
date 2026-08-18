import { describe, expect, it } from 'vitest'
import { classifyImageDeploymentSupport } from '../app/utils/image-deployment-support'

describe('image deployment support classification', () => {
  it.each(['wim', 'esd'] as const)(
    'classifies verified %s images as eligible for automatic deployment',
    (format) => {
      expect(classifyImageDeploymentSupport(format, true)).toBe('automatic')
    }
  )

  it('keeps a verified GHO image manual-only', () => {
    expect(classifyImageDeploymentSupport('gho', true)).toBe('manual')
  })

  it.each(['swm'] as const)(
    'keeps %s images catalog-only',
    (format) => {
      expect(classifyImageDeploymentSupport(format, true)).toBe('catalog-only')
      expect(classifyImageDeploymentSupport(format, false)).toBe('catalog-only')
    }
  )

  it.each(['gho', 'wim', 'esd'] as const)(
    'requires verification before %s images become eligible',
    (format) => {
      expect(classifyImageDeploymentSupport(format, false))
        .toBe('verification-required')
    }
  )
})
