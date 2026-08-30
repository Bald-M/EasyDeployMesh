import { describe, expect, it } from 'vitest'
import type {
  Disk,
  ImageArtifact,
  InstallerCapability,
  RegisteredDevice
} from '../app/types/deployment'
import {
  defaultLinuxTargetDisk,
  linuxDeploymentConfirmationKey,
  linuxInstallOptionsFor,
  linuxTargetBlockers,
  parseLinuxSshAuthorizedKeys,
  supportedUbuntuInstallerCapability,
  validateLinuxInstallForm
} from '../app/utils/linux-install'

const gib = 1024 ** 3
const sha256 = 'a'.repeat(64)
const sshKey = `ssh-ed25519 ${'A'.repeat(40)} operator@example`

function capability(overrides: Partial<InstallerCapability> = {}): InstallerCapability {
  return {
    deployable: true,
    distribution: 'ubuntu',
    release: '24.04',
    architecture: 'x86_64',
    profile: 'ubuntu_autoinstall',
    profileVersion: 1,
    kernel: { path: 'casper/vmlinuz', sizeBytes: 16_000_000, sha256 },
    initrd: { path: 'casper/initrd', sizeBytes: 80_000_000, sha256 },
    minimumMemoryBytes: 2 * gib,
    minimumDiskBytes: 25 * gib,
    blockedReason: null,
    ...overrides
  }
}

function image(overrides: Partial<ImageArtifact> = {}): ImageArtifact {
  return {
    id: 'image-1',
    name: 'ubuntu-24.04.3-live-server-amd64.iso',
    format: 'iso',
    sourcePath: '/managed/image.iso',
    sizeBytes: 3 * gib,
    sha256,
    spans: [],
    verified: true,
    installerCapability: capability(),
    createdAt: '2026-08-28T00:00:00Z',
    ...overrides
  }
}

function disk(overrides: Partial<Disk> = {}): Disk {
  return {
    id: 'disk-1',
    model: 'NVMe disk',
    serial: 'NVME-SERIAL-1',
    sizeBytes: 64 * gib,
    isSystem: false,
    ...overrides
  }
}

function device(overrides: Partial<RegisteredDevice['device']> = {}): RegisteredDevice {
  return {
    device: {
      id: 'device-1',
      hostname: 'old-host',
      macAddress: '02:00:00:ab:cd:ef',
      ipAddress: '192.0.2.10',
      model: null,
      serial: null,
      cpuModel: null,
      physicalCoreCount: null,
      logicalProcessorCount: 4,
      memoryBytes: 4 * gib,
      systemDetails: {
        osName: null,
        osVersion: null,
        uptimeSeconds: null,
        motherboard: null,
        memoryModules: [],
        gpus: [],
        displays: [],
        audioDevices: [],
        networkAdapters: []
      },
      architecture: 'x86_64',
      bootMode: 'uefi',
      disks: [disk()],
      lastSeenAt: '2026-08-28T00:00:00Z',
      ...overrides
    },
    agentVersion: '0.2.6',
    firstSeenAt: '2026-08-28T00:00:00Z',
    online: true
  }
}

describe('Ubuntu installer capability', () => {
  it('accepts only the verified supported Ubuntu Server v1 profile', () => {
    expect(supportedUbuntuInstallerCapability(image())).toEqual(capability())
    expect(supportedUbuntuInstallerCapability(image({ verified: false }))).toBeNull()
    expect(supportedUbuntuInstallerCapability(image({ sha256: null }))).toBeNull()
    expect(supportedUbuntuInstallerCapability(image({
      installerCapability: capability({ release: '24.10' })
    }))).toBeNull()
    expect(supportedUbuntuInstallerCapability(image({
      installerCapability: capability({ deployable: false, blockedReason: 'unsupported ISO' })
    }))).toBeNull()
  })
})

describe('Linux target eligibility', () => {
  it('defaults only an unambiguous single disk with a strong serial', () => {
    const onlyDisk = disk()
    expect(defaultLinuxTargetDisk([onlyDisk])).toBe(onlyDisk)
    expect(defaultLinuxTargetDisk([disk({ serial: null })])).toBeNull()
    expect(defaultLinuxTargetDisk([
      disk(),
      disk({ id: 'disk-2', serial: 'NVME-SERIAL-2' })
    ])).toBeNull()
  })

  it('accepts an amd64 UEFI target with a strong disk serial and enough capacity', () => {
    expect(linuxTargetBlockers(device(), disk(), capability())).toEqual([])
  })

  it('fails closed for every unsupported or unknown hardware requirement', () => {
    expect(linuxTargetBlockers(device({
      architecture: 'aarch64',
      bootMode: 'legacy_bios',
      memoryBytes: gib
    }), disk({ serial: ' ', sizeBytes: 20 * gib }), capability())).toEqual([
      'architecture',
      'boot_mode',
      'memory',
      'disk_serial',
      'disk_capacity'
    ])
  })
})

describe('Linux installer inputs', () => {
  it('normalizes public keys and derives a stable per-device hostname', () => {
    expect(parseLinuxSshAuthorizedKeys(`\n${sshKey}\n${sshKey} backup\n`)).toEqual([
      sshKey,
      `${sshKey} backup`
    ])
    expect(linuxInstallOptionsFor(device(), {
      hostnamePrefix: 'lab-node',
      username: 'operator',
      sshPublicKeys: sshKey
    })).toEqual({
      hostname: 'lab-node-abcdef',
      username: 'operator',
      sshAuthorizedKeys: [sshKey]
    })
  })

  it('mirrors the host validation for hostname prefix, account and keys', () => {
    expect(validateLinuxInstallForm({
      hostnamePrefix: 'Lab Node',
      username: 'root',
      sshPublicKeys: 'not-a-public-key'
    })).toEqual(['hostname_prefix', 'username', 'ssh_public_key'])
    expect(validateLinuxInstallForm({
      hostnamePrefix: 'lab',
      username: 'operator',
      sshPublicKeys: `${sshKey}\n${sshKey}`
    })).toEqual(['duplicate_ssh_public_key'])
  })
})

describe('Linux deployment confirmation binding', () => {
  const form = {
    hostnamePrefix: 'lab',
    username: 'operator',
    sshPublicKeys: sshKey
  }

  it('binds the image hash, device, exact target disk and normalized Linux config', () => {
    const baseline = linuxDeploymentConfirmationKey(image(), [
      { entry: device(), disk: disk() }
    ], form)

    expect(baseline).not.toBe(linuxDeploymentConfirmationKey(
      image({ sha256: 'b'.repeat(64) }),
      [{ entry: device(), disk: disk() }],
      form
    ))
    expect(baseline).not.toBe(linuxDeploymentConfirmationKey(
      image(),
      [{ entry: device(), disk: disk({ serial: 'NVME-SERIAL-2' }) }],
      form
    ))
    expect(baseline).not.toBe(linuxDeploymentConfirmationKey(
      image(),
      [{ entry: device({ id: 'device-2' }), disk: disk() }],
      form
    ))
    expect(baseline).not.toBe(linuxDeploymentConfirmationKey(
      image(),
      [{ entry: device(), disk: disk() }],
      { ...form, username: 'installer' }
    ))
  })
})
