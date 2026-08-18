import { describe, expect, it } from 'vitest'
import { preferredIpv4Interface, suggestedPxeNetwork } from '../app/utils/network'

const wifi = { name: 'en0', address: '192.168.10.8', netmask: '255.255.255.0', isLoopback: false, isUp: true }

describe('PXE network suggestions', () => {
  it('fills a pool inside the selected Wi-Fi subnet', () => {
    expect(suggestedPxeNetwork(wifi)).toEqual({
      subnetMask: '255.255.255.0',
      poolStart: '192.168.10.100',
      poolEnd: '192.168.10.200'
    })
  })

  it('supports subnets other than /24 and excludes the server address', () => {
    expect(suggestedPxeNetwork({ ...wifi, address: '10.0.0.105', netmask: '255.255.255.128' })).toEqual({
      subnetMask: '255.255.255.128',
      poolStart: '10.0.0.106',
      poolEnd: '10.0.0.126'
    })
  })

  it('prefers an active physical interface over a VPN', () => {
    expect(preferredIpv4Interface([
      { ...wifi, name: 'utun4', address: '10.8.0.2' },
      wifi
    ])?.address).toBe(wifi.address)
  })
})
