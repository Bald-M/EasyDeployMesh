import { defineStore } from 'pinia'
import { createJob, getJobs, removeJob, transitionJob } from '~/services/deployment'
import type { CreateDeploymentJob, DeploymentJob } from '~/types/deployment'
import { createLatestRefresh } from '~/utils/refresh-coordination'

export const useJobStore = defineStore('jobs', () => {
  const jobs = ref<DeploymentJob[]>([])
  const loading = ref(false)
  const lastError = ref<string | null>(null)

  const queuedCount = computed(() =>
    jobs.value.filter(job => job.state === 'waiting').length
  )

  const latest = createLatestRefresh(getJobs, (next) => { jobs.value = next })

  // Background refreshes (polling) never toggle the user-visible `loading`.
  async function refresh(options: { background?: boolean } = {}) {
    if (!options.background) loading.value = true
    lastError.value = null

    try {
      await latest.run()
    } catch (error) {
      lastError.value = String(error)
    } finally {
      if (!options.background) loading.value = false
    }
  }

  async function enqueue(request: CreateDeploymentJob) {
    loading.value = true
    lastError.value = null
    try {
      const queued = await createJob(request)
      latest.invalidate()
      jobs.value = [queued, ...jobs.value.filter(job => job.id !== queued.id)]
      return queued
    } catch (error) {
      lastError.value = String(error)
      throw error
    } finally {
      loading.value = false
    }
  }

  async function remove(id: string) {
    lastError.value = null
    try {
      const removed = await removeJob(id)
      latest.invalidate()
      if (removed) jobs.value = jobs.value.filter(job => job.id !== id)
      return removed
    } catch (error) {
      lastError.value = String(error)
      throw error
    }
  }

  async function setState(id: string, state: DeploymentJob['state']) {
    lastError.value = null
    try {
      const updated = await transitionJob(id, state)
      latest.invalidate()
      jobs.value = jobs.value.map(job => job.id === id ? updated : job)
      return updated
    } catch (error) {
      lastError.value = String(error)
      throw error
    }
  }

  return {
    jobs,
    loading,
    lastError,
    queuedCount,
    refresh,
    enqueue,
    setState,
    remove
  }
})
