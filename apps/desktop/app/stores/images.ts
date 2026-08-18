import { defineStore } from 'pinia'
import {
  getImages,
  importImage,
  pickImageFiles,
  removeImage
} from '~/services/deployment'
import type { ImageArtifact } from '~/types/deployment'

export const useImageStore = defineStore('images', () => {
  const images = ref<ImageArtifact[]>([])
  const loading = ref(false)
  const importing = ref(false)
  const lastError = ref<string | null>(null)

  const verifiedCount = computed(() =>
    images.value.filter(image => image.verified).length
  )

  async function refresh() {
    loading.value = true
    lastError.value = null

    try {
      images.value = await getImages()
    } catch (error) {
      lastError.value = String(error)
    } finally {
      loading.value = false
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
      images.value = await getImages()
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
