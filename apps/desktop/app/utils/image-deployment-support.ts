import type { ImageFormat } from '~/types/deployment'

export type ImageDeploymentSupport =
  | 'automatic'
  | 'manual'
  | 'verification-required'
  | 'catalog-only'

export function classifyImageDeploymentSupport(
  format: ImageFormat,
  verified: boolean
): ImageDeploymentSupport {
  if (format === 'swm') {
    return 'catalog-only'
  }

  if (format === 'gho' && verified) {
    return 'manual'
  }

  return verified ? 'automatic' : 'verification-required'
}
