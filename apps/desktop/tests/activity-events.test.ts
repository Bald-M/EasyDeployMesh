import { describe, expect, it } from 'vitest'
import { groupActivityEvents, severitiesForStatus, sourcesForFilter } from '../app/utils/activity-events'
import type { ActivityEvent } from '../app/types/runtime'

function event(overrides: Partial<ActivityEvent> = {}): ActivityEvent {
  return { id: crypto.randomUUID(), occurredAt: '2026-08-17T03:00:00Z', source: 'device', kind: 'pxe_request_accepted', severity: 'info', subject: { id: 'mac', name: 'mac' }, details: { architecture: 'x86_64' }, rawMessage: null, ...overrides }
}

describe('activity event presentation', () => {
  it('maps human status and source filters to machine values', () => {
    expect(severitiesForStatus('normal')).toEqual(['info', 'success'])
    expect(severitiesForStatus('abnormal')).toEqual(['warning', 'error'])
    expect(sourcesForFilter('deployment')).toEqual(['deployment'])
  })

  it('merges adjacent identical events within 60 seconds', () => {
    const grouped = groupActivityEvents([
      event({ id: '1', occurredAt: '2026-08-17T03:01:00Z' }),
      event({ id: '2', occurredAt: '2026-08-17T03:00:01Z' })
    ])
    expect(grouped).toHaveLength(1)
    expect(grouped[0]?.count).toBe(2)
  })

  it('does not merge events outside the window or for different subjects', () => {
    expect(groupActivityEvents([
      event({ occurredAt: '2026-08-17T03:02:00Z' }),
      event({ occurredAt: '2026-08-17T03:00:00Z' })
    ])).toHaveLength(2)
    expect(groupActivityEvents([
      event(),
      event({ subject: { id: 'other', name: 'other' } })
    ])).toHaveLength(2)
  })
})
