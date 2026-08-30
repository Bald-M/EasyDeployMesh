import type { ImageFormat, InstallerCapability, Operation } from '~/types/deployment'

export type ImageDeploymentSupport =
  | 'automatic'
  | 'manual'
  | 'verification-required'
  | 'catalog-only'

export function classifyImageDeploymentSupport(
  format: ImageFormat,
  verified: boolean,
  installerCapability: InstallerCapability | null = null
): ImageDeploymentSupport {
  if (format === 'swm') {
    return 'catalog-only'
  }

  if (format === 'gho' && verified) {
    return 'manual'
  }

  if (format === 'iso') {
    return verified && installerCapability?.deployable === true
      ? 'automatic'
      : 'verification-required'
  }

  return verified ? 'automatic' : 'verification-required'
}

export function deploymentOperationForImageFormat(format: ImageFormat): Operation | null {
  if (format === 'iso') return 'install_linux'
  if (format === 'gho') return 'deploy_gho'
  if (format === 'wim' || format === 'esd') return 'deploy_wim'
  return null
}
