import { defineStore } from 'pinia'
import { createJob, getJobs, removeJob, transitionJob } from '~/services/deployment'
import type { CreateDeploymentJob, DeploymentJob } from '~/types/deployment'

export const useJobStore = defineStore('jobs', () => {
  const jobs = ref<DeploymentJob[]>([])
  const loading = ref(false)
  const lastError = ref<string | null>(null)

  const queuedCount = computed(() =>
    jobs.value.filter(job => job.state === 'waiting').length
  )

  async function refresh() {
    loading.value = true
    lastError.value = null

    try {
      jobs.value = await getJobs()
    } catch (error) {
      lastError.value = String(error)
    } finally {
      loading.value = false
    }
  }

  async function enqueue(request: CreateDeploymentJob) {
    loading.value = true
    lastError.value = null
    try {
      const queued = await createJob(request)
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
