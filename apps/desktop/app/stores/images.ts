import { defineStore } from 'pinia'
import {
  getImages,
  importImage,
  pickImageFiles,
  removeImage
} from '~/services/deployment'
import type { ImageArtifact } from '~/types/deployment'
import { createLatestRefresh } from '~/utils/refresh-coordination'

export const useImageStore = defineStore('images', () => {
  const images = ref<ImageArtifact[]>([])
  const loading = ref(false)
  const importing = ref(false)
  const lastError = ref<string | null>(null)

  const verifiedCount = computed(() =>
    images.value.filter(image => image.verified).length
  )

  const latest = createLatestRefresh(getImages, (next) => { images.value = next })

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

  async function importFromPicker(): Promise<number> {
    const paths = await pickImageFiles()
    if (!paths.length) {
      return 0
    }

    importing.value = true
    lastError.value = null
    let imported = 0

    try {
      for (const path of paths) {
        await importImage(path)
        imported += 1
      }
      latest.invalidate()
      await latest.run()
      return imported
    } catch (error) {
      lastError.value = String(error)
      throw error
    } finally {
      importing.value = false
    }
  }

  async function remove(id: string) {
    lastError.value = null
    try {
      const removed = await removeImage(id)
      latest.invalidate()
      if (removed) {
        images.value = images.value.filter(image => image.id !== id)
      }
      return removed
    } catch (error) {
      lastError.value = String(error)
      throw error
    }
  }

  return {
    images,
    loading,
    importing,
    lastError,
    verifiedCount,
    refresh,
    importFromPicker,
    remove
  }
})
