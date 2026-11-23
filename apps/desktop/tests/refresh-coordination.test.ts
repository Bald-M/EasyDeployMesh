import { afterEach, describe, expect, it, vi } from 'vitest'
import { createLatestRefresh, startVisiblePolling } from '../app/utils/refresh-coordination'

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => { resolve = done })
  return { promise, resolve }
}

describe('createLatestRefresh', () => {
  it('shares one in-flight request between concurrent callers', async () => {
    const pending = deferred<number>()
    const load = vi.fn(() => pending.promise)
    const applied: number[] = []
    const refresh = createLatestRefresh(load, value => applied.push(value))

    const first = refresh.run()
    const second = refresh.run()
    expect(second).toBe(first)
    pending.resolve(1)
    await first
    expect(load).toHaveBeenCalledTimes(1)
    expect(applied).toEqual([1])
  })

  it('drops a response that started before a newer write', async () => {
    const stale = deferred<string>()
    const fresh = deferred<string>()
    const load = vi.fn().mockReturnValueOnce(stale.promise).mockReturnValueOnce(fresh.promise)
    let state = 'initial'
    const refresh = createLatestRefresh(load, (value: string) => { state = value })

    const polling = refresh.run()
    refresh.invalidate()
    state = 'written locally'
    const afterWrite = refresh.run()
    expect(afterWrite).not.toBe(polling)

    stale.resolve('stale poll')
    await polling
    expect(state).toBe('written locally')

    fresh.resolve('fresh poll')
    await afterWrite
    expect(state).toBe('fresh poll')
  })

  it('does not clear a newer in-flight request when a stale one settles', async () => {
    const stale = deferred<number>()
    const fresh = deferred<number>()
    const load = vi.fn().mockReturnValueOnce(stale.promise).mockReturnValueOnce(fresh.promise)
    const refresh = createLatestRefresh(load, () => {})

    const old = refresh.run()
    refresh.invalidate()
    const current = refresh.run()
    stale.resolve(0)
    await old
    expect(refresh.run()).toBe(current)
    fresh.resolve(1)
    await current
  })

  it('propagates load errors and allows a retry', async () => {
    const load = vi.fn().mockRejectedValueOnce(new Error('boom')).mockResolvedValueOnce(2)
    const applied: number[] = []
    const refresh = createLatestRefresh(load, (value: number) => applied.push(value))

    await expect(refresh.run()).rejects.toThrow('boom')
    await refresh.run()
    expect(applied).toEqual([2])
  })
})

describe('startVisiblePolling', () => {
  afterEach(() => {
    vi.useRealTimers()
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => false })
  })

  function setHidden(hidden: boolean) {
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden })
    document.dispatchEvent(new Event('visibilitychange'))
  }

  it('skips ticks while hidden and refreshes once when visible again', () => {
    vi.useFakeTimers()
    const task = vi.fn()
    const stop = startVisiblePolling(task, 1000)

    vi.advanceTimersByTime(1000)
    expect(task).toHaveBeenCalledTimes(1)

    setHidden(true)
    vi.advanceTimersByTime(5000)
    expect(task).toHaveBeenCalledTimes(1)

    setHidden(false)
    expect(task).toHaveBeenCalledTimes(2)

    stop()
    vi.advanceTimersByTime(5000)
    setHidden(false)
    expect(task).toHaveBeenCalledTimes(2)
  })
})
