import { isAppLocale, resolveLocale, type AppLocale } from '~/utils/locale'

const storageKey = 'easydeploymesh.locale'

export function useAppLocale() {
  const { locale, setLocale } = useI18n()

  const currentLocale = computed<AppLocale>(() =>
    resolveLocale(locale.value)
  )

  async function changeLocale(nextLocale: string) {
    if (!isAppLocale(nextLocale)) {
      return
    }

    await setLocale(nextLocale)

    if (import.meta.client) {
      localStorage.setItem(storageKey, nextLocale)
    }
  }

  async function initializeLocale() {
    if (!import.meta.client) {
      return
    }

    const stored = localStorage.getItem(storageKey)
    const nextLocale = isAppLocale(stored)
      ? stored
      : resolveLocale(navigator.language)

    await changeLocale(nextLocale)
  }

  return {
    currentLocale,
    changeLocale,
    initializeLocale
  }
}

