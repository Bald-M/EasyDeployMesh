import assert from 'node:assert/strict'
import test from 'node:test'
import { desktopTarget, desktopTargets } from './desktop-targets.mjs'

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

test('rejects an unknown target', () => {
  assert.throws(() => desktopTarget('solaris-sparc'), /Unknown desktop target/)
})
