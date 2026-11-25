// Coordinates a repeated read so concurrent callers share one request and a
// response that started before a newer write (`invalidate`) is never applied.
export function createLatestRefresh<T>(load: () => Promise<T>, apply: (value: T) => void) {
  let inFlight: Promise<void> | null = null
  let generation = 0

  function run(): Promise<void> {
    if (inFlight) return inFlight
    const started = generation
    const request: Promise<void> = load()
      .then((value) => {
        if (started === generation) apply(value)
      })
      .finally(() => {
        if (inFlight === request) inFlight = null
      })
    inFlight = request
    return request
  }

  // Call before applying a local write: any read already in flight is dropped
  // and the next `run` starts a fresh request instead of reusing it.
  function invalidate() {
    generation += 1
    inFlight = null
  }

  return { run, invalidate }
}

// Runs `task` every `intervalMs` while the document is visible; pauses while
// hidden and runs once immediately when it becomes visible again.
export function startVisiblePolling(task: () => unknown, intervalMs: number, doc: Document = document) {
  const tick = () => {
    if (!doc.hidden) void task()
  }
  const onVisibilityChange = () => {
    if (!doc.hidden) void task()
  }
  const timer = setInterval(tick, intervalMs)
  doc.addEventListener('visibilitychange', onVisibilityChange)
  return () => {
    clearInterval(timer)
    doc.removeEventListener('visibilitychange', onVisibilityChange)
  }
}
