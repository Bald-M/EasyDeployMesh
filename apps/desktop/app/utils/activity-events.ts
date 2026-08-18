import type { ActivityEvent, ActivitySeverity, ActivitySource } from '~/types/runtime'

export type ActivityStatusFilter = 'all' | 'normal' | 'abnormal'

export interface ActivityGroup extends ActivityEvent {
  count: number
  firstOccurredAt: string
  lastOccurredAt: string
}

export function severitiesForStatus(status: ActivityStatusFilter): ActivitySeverity[] {
  if (status === 'normal') return ['info', 'success']
  if (status === 'abnormal') return ['warning', 'error']
  return []
}

export function sourcesForFilter(source: ActivitySource | 'all'): ActivitySource[] {
  return source === 'all' ? [] : [source]
}

function groupingKey(event: ActivityEvent): string {
  return JSON.stringify([event.kind, event.subject?.id ?? '', event.details])
}

export function groupActivityEvents(events: ActivityEvent[], windowMs = 60_000): ActivityGroup[] {
  const groups: ActivityGroup[] = []
  for (const event of events) {
    const previous = groups.at(-1)
    const eventTime = new Date(event.occurredAt).getTime()
    const canMerge = previous
      && groupingKey(previous) === groupingKey(event)
      && new Date(previous.firstOccurredAt).getTime() - eventTime <= windowMs
    if (canMerge) {
      previous.count += 1
      previous.firstOccurredAt = event.occurredAt
      continue
    }
    groups.push({ ...event, count: 1, firstOccurredAt: event.occurredAt, lastOccurredAt: event.occurredAt })
  }
  return groups
}
