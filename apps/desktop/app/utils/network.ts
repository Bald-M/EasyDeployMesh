import type { NetworkInterfaceSummary } from '~/types/runtime'

function parseIpv4(value: string): number | null {
  const octets = value.split('.').map(Number)
  if (octets.length !== 4 || octets.some(value => !Number.isInteger(value) || value < 0 || value > 255)) return null
  return octets.reduce((result, octet) => ((result << 8) | octet) >>> 0, 0)
}

function formatIpv4(value: number): string {
  return [24, 16, 8, 0].map(shift => (value >>> shift) & 255).join('.')
}

export function suggestedPxeNetwork(networkInterface: NetworkInterfaceSummary) {
  const address = parseIpv4(networkInterface.address)
  const mask = parseIpv4(networkInterface.netmask)
  if (address === null || mask === null) return null

  const network = (address & mask) >>> 0
  const broadcast = (network | (~mask >>> 0)) >>> 0
  if (broadcast - network < 3) return null

  const firstHost = network + 1
  const lastHost = broadcast - 1
  const preferredStart = Math.min(network + 100, lastHost)
  const preferredEnd = Math.min(network + 200, lastHost)
  let poolStart = Math.max(firstHost, preferredStart)
  let poolEnd = Math.max(poolStart, preferredEnd)

  // The PXE server itself must not be part of the leased address range.
  if (address >= poolStart && address <= poolEnd) {
    if (address - poolStart >= poolEnd - address) poolEnd = address - 1
    else poolStart = address + 1
  }
  if (poolStart > poolEnd) return null

  return {
    subnetMask: networkInterface.netmask,
    poolStart: formatIpv4(poolStart),
    poolEnd: formatIpv4(poolEnd)
  }
}

export function preferredIpv4Interface(interfaces: NetworkInterfaceSummary[]) {
  const candidates = interfaces.filter(item => item.isUp && !item.isLoopback && parseIpv4(item.address) !== null)
  return candidates.sort((left, right) => interfaceScore(right) - interfaceScore(left))[0] ?? null
}

function interfaceScore(item: NetworkInterfaceSummary): number {
  const address = parseIpv4(item.address) ?? 0
  const privateAddress = (address >>> 24) === 10
    || (address >>> 20) === 0xAC1
    || (address >>> 16) === 0xC0A8
  const likelyPhysical = /^(en\d+|eth\d+|wlan\d+|wi-?fi|ethernet)/i.test(item.name)
  const likelyVirtual = /(bridge|docker|veth|utun|tun|tap|vpn|virtual|vmnet)/i.test(item.name)
  return (privateAddress ? 4 : 0) + (likelyPhysical ? 2 : 0) - (likelyVirtual ? 4 : 0)
}
