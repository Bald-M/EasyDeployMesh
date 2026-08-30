import type {
  Disk,
  ImageArtifact,
  InstallerBootAsset,
  InstallerCapability,
  LinuxInstallOptions,
  RegisteredDevice
} from '~/types/deployment'

const minimumUbuntuMemoryBytes = 2 * 1024 ** 3
const minimumUbuntuDiskBytes = 25 * 1024 ** 3
const sha256Pattern = /^[a-f\d]{64}$/i
const supportedSshAlgorithms = new Set([
  'ssh-ed25519',
  'ssh-rsa',
  'ecdsa-sha2-nistp256',
  'ecdsa-sha2-nistp384',
  'ecdsa-sha2-nistp521',
  'sk-ssh-ed25519@openssh.com',
  'sk-ecdsa-sha2-nistp256@openssh.com'
])

export interface LinuxInstallForm {
  hostnamePrefix: string
  username: string
  sshPublicKeys: string
}

export type LinuxInstallFormError =
  | 'hostname_prefix'
  | 'username'
  | 'ssh_public_key_count'
  | 'ssh_public_key'
  | 'duplicate_ssh_public_key'

export type LinuxTargetBlocker =
  | 'architecture'
  | 'boot_mode'
  | 'memory'
  | 'disk_serial'
  | 'disk_capacity'

export interface LinuxDeploymentTargetSelection {
  entry: RegisteredDevice
  disk: Disk | null
}

export function defaultLinuxTargetDisk(disks: readonly Disk[]): Disk | null {
  if (disks.length !== 1) return null
  const disk = disks[0]
  return disk?.serial?.trim() ? disk : null
}

function installerAssetIsValid(asset: InstallerBootAsset) {
  return asset.path.trim().length > 0
    && asset.sizeBytes > 0
    && sha256Pattern.test(asset.sha256)
}

export function supportedUbuntuInstallerCapability(
  image: ImageArtifact | null
): InstallerCapability | null {
  const capability = image?.installerCapability
  if (!image
    || image.format !== 'iso'
    || !image.verified
    || !image.sha256
    || !sha256Pattern.test(image.sha256)
    || !capability
    || !capability.deployable
    || capability.blockedReason !== null
    || capability.distribution !== 'ubuntu'
    || capability.release !== '24.04'
    || capability.architecture !== 'x86_64'
    || capability.profile !== 'ubuntu_autoinstall'
    || capability.profileVersion !== 1
    || capability.minimumMemoryBytes < minimumUbuntuMemoryBytes
    || capability.minimumDiskBytes < minimumUbuntuDiskBytes
    || !installerAssetIsValid(capability.kernel)
    || !installerAssetIsValid(capability.initrd)) {
    return null
  }
  return capability
}

export function linuxTargetBlockers(
  entry: RegisteredDevice,
  disk: Disk | null,
  capability: InstallerCapability
): LinuxTargetBlocker[] {
  const blockers: LinuxTargetBlocker[] = []
  if (entry.device.architecture !== 'x86_64'
    || capability.architecture !== 'x86_64') {
    blockers.push('architecture')
  }
  if (entry.device.bootMode !== 'uefi') blockers.push('boot_mode')
  if (entry.device.memoryBytes < capability.minimumMemoryBytes) blockers.push('memory')
  if (!disk?.serial?.trim()) blockers.push('disk_serial')
  if (!disk || disk.sizeBytes < capability.minimumDiskBytes) blockers.push('disk_capacity')
  return blockers
}

export function parseLinuxSshAuthorizedKeys(value: string): string[] {
  return value
    .split(/\r?\n/u)
    .map(key => key.trim())
    .filter(Boolean)
}

function validHostnamePrefix(prefix: string) {
  return prefix.length >= 1
    && prefix.length <= 56
    && /^[a-z\d](?:[a-z\d-]*[a-z\d])?$/u.test(prefix)
}

function validLinuxUsername(username: string) {
  return username.length >= 1
    && username.length <= 32
    && username !== 'root'
    && /^[a-z][a-z\d_-]*$/u.test(username)
}

function validSshPublicKey(key: string) {
  if (key.length === 0 || key.length > 16 * 1024 || !/^[\x20-\x7e]+$/u.test(key)) {
    return false
  }
  const [algorithm, encoded] = key.split(/\s+/u)
  return Boolean(
    algorithm
    && encoded
    && supportedSshAlgorithms.has(algorithm)
    && encoded.length >= 16
    && /^[A-Za-z\d+/=]+$/u.test(encoded)
  )
}

export function validateLinuxInstallForm(form: LinuxInstallForm): LinuxInstallFormError[] {
  const errors: LinuxInstallFormError[] = []
  if (!validHostnamePrefix(form.hostnamePrefix)) errors.push('hostname_prefix')
  if (!validLinuxUsername(form.username)) errors.push('username')

  const keys = parseLinuxSshAuthorizedKeys(form.sshPublicKeys)
  if (keys.length < 1 || keys.length > 16) {
    errors.push('ssh_public_key_count')
  } else if (keys.some(key => !validSshPublicKey(key))) {
    errors.push('ssh_public_key')
  } else if (new Set(keys).size !== keys.length) {
    errors.push('duplicate_ssh_public_key')
  }
  return errors
}

function deviceHostnameSuffix(entry: RegisteredDevice) {
  const macSuffix = entry.device.macAddress.replace(/[^a-f\d]/giu, '').slice(-6).toLowerCase()
  if (macSuffix.length === 6) return macSuffix
  return entry.device.id.replace(/[^a-z\d]/giu, '').slice(-6).toLowerCase().padStart(6, '0')
}

export function linuxInstallOptionsFor(
  entry: RegisteredDevice,
  form: LinuxInstallForm
): LinuxInstallOptions {
  return {
    hostname: `${form.hostnamePrefix}-${deviceHostnameSuffix(entry)}`,
    username: form.username,
    sshAuthorizedKeys: parseLinuxSshAuthorizedKeys(form.sshPublicKeys)
  }
}

export function linuxDeploymentConfirmationKey(
  image: ImageArtifact,
  targets: readonly LinuxDeploymentTargetSelection[],
  form: LinuxInstallForm
) {
  return JSON.stringify({
    kind: 'ubuntu_autoinstall_v1',
    image: {
      id: image.id,
      sha256: image.sha256,
      sizeBytes: image.sizeBytes,
      verified: image.verified,
      installerCapability: image.installerCapability ?? null
    },
    policy: {
      network: 'dhcp',
      storage: 'whole_disk_gpt_ext4'
    },
    targets: targets
      .map(({ entry, disk }) => ({
        deviceId: entry.device.id,
        macAddress: entry.device.macAddress,
        architecture: entry.device.architecture,
        bootMode: entry.device.bootMode,
        memoryBytes: entry.device.memoryBytes,
        disk: disk
          ? {
              id: disk.id,
              model: disk.model,
              serial: disk.serial,
              sizeBytes: disk.sizeBytes
            }
          : null,
        linuxInstall: linuxInstallOptionsFor(entry, form)
      }))
      .sort((left, right) => left.deviceId.localeCompare(right.deviceId))
  })
}
