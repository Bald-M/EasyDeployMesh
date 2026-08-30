import { describe, expect, it } from 'vitest'
import {
  classifyImageDeploymentSupport,
  deploymentOperationForImageFormat
} from '../app/utils/image-deployment-support'

describe('image deployment support classification', () => {
  const ubuntuInstallerCapability = {
    deployable: true,
    distribution: 'ubuntu',
    release: '24.04',
    architecture: 'x86_64',
    profile: 'ubuntu_autoinstall',
    profileVersion: 1,
    kernel: {
      path: 'casper/vmlinuz',
      sizeBytes: 16_000_000,
      sha256: 'kernel-sha256'
    },
    initrd: {
      path: 'casper/initrd',
      sizeBytes: 80_000_000,
      sha256: 'initrd-sha256'
    },
    minimumMemoryBytes: 4_294_967_296,
    minimumDiskBytes: 26_843_545_600,
    blockedReason: null
  } as const

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

  it('classifies only a verified deployable ISO as automatic', () => {
    expect(classifyImageDeploymentSupport('iso', true, ubuntuInstallerCapability))
      .toBe('automatic')
    expect(classifyImageDeploymentSupport('iso', false, ubuntuInstallerCapability))
      .toBe('verification-required')
    expect(classifyImageDeploymentSupport('iso', true, {
      ...ubuntuInstallerCapability,
      deployable: false,
      blockedReason: 'missing installer kernel'
    })).toBe('verification-required')
  })

  it('does not treat an ISO without installer capability as deployable', () => {
    expect(classifyImageDeploymentSupport('iso', true, null))
      .toBe('verification-required')
  })

  it('uses the dedicated Linux installation operation for ISO images', () => {
    expect(deploymentOperationForImageFormat('iso')).toBe('install_linux')
    expect(deploymentOperationForImageFormat('gho')).toBe('deploy_gho')
    expect(deploymentOperationForImageFormat('wim')).toBe('deploy_wim')
    expect(deploymentOperationForImageFormat('swm')).toBeNull()
  })
})
