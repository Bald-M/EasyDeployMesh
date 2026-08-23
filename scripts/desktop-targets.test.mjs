import assert from 'node:assert/strict'
import test from 'node:test'
import {
  desktopBuildSelectors,
  desktopBuildTargets,
  desktopTarget,
  desktopTargets,
} from './desktop-targets.mjs'

test('defines every supported desktop installer architecture', () => {
  assert.deepEqual(Object.keys(desktopTargets), [
    'macos-arm64',
    'macos-x64',
    'windows-arm64',
    'windows-x86',
    'windows-x64',
    'linux-arm64',
    'linux-x64',
  ])
  assert.equal(new Set(Object.values(desktopTargets).map(({ artifactLabel }) => artifactLabel)).size, 7)
})

test('uses platform-native installer formats', () => {
  assert.deepEqual(
    [...new Set(Object.values(desktopTargets).filter(({ platform }) => platform === 'macos').map(({ extension }) => extension))],
    ['.dmg'],
  )
  assert.deepEqual(
    [...new Set(Object.values(desktopTargets).filter(({ platform }) => platform === 'windows').map(({ extension }) => extension))],
    ['.exe'],
  )
  assert.deepEqual(
    [...new Set(Object.values(desktopTargets).filter(({ platform }) => platform === 'linux').map(({ extension }) => extension))],
    ['.AppImage'],
  )
})

test('maps every Windows target to its cargo-xwin SDK architecture', () => {
  assert.deepEqual(
    Object.fromEntries(
      Object.entries(desktopTargets)
        .filter(([, { platform }]) => platform === 'windows')
        .map(([name, { xwinArch }]) => [name, xwinArch]),
    ),
    {
      'windows-arm64': 'aarch64',
      'windows-x86': 'x86',
      'windows-x64': 'x86_64',
    },
  )
})

test('builds native installers and cross-compiled Windows installers by default', () => {
  assert.deepEqual(desktopBuildTargets('darwin'), [
    'macos-arm64',
    'macos-x64',
    'windows-arm64',
    'windows-x86',
    'windows-x64',
  ])
  assert.deepEqual(desktopBuildTargets('linux'), [
    'linux-arm64',
    'linux-x64',
    'windows-arm64',
    'windows-x86',
    'windows-x64',
  ])
  assert.deepEqual(desktopBuildTargets('win32'), [
    'windows-arm64',
    'windows-x86',
    'windows-x64',
  ])
})

test('accepts platform, architecture, and multiple build selectors', () => {
  assert.deepEqual(desktopBuildTargets('darwin', ['windows']), [
    'windows-arm64',
    'windows-x86',
    'windows-x64',
  ])
  assert.deepEqual(desktopBuildTargets('darwin', ['macos-x64', 'windows-x64']), [
    'macos-x64',
    'windows-x64',
  ])
  assert.deepEqual(desktopBuildTargets('linux', ['linux', 'windows-x64']), [
    'linux-arm64',
    'linux-x64',
    'windows-x64',
  ])
})

test('ignores pnpm argument separators before resolving build selectors', () => {
  assert.deepEqual(desktopBuildSelectors(['--', 'windows-x64']), ['windows-x64'])
  assert.deepEqual(desktopBuildSelectors(['windows-x86']), ['windows-x86'])
})

test('rejects targets that require a different native host', () => {
  assert.throws(() => desktopBuildTargets('darwin', ['linux-x64']), /cannot be built on darwin/)
  assert.throws(() => desktopBuildTargets('linux', ['macos']), /cannot be built on linux/)
})

test('rejects an unknown target', () => {
  assert.throws(() => desktopTarget('solaris-sparc'), /Unknown desktop target/)
})
