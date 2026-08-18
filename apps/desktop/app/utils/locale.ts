export const supportedLocales = ['zh-CN', 'en-US'] as const

export type AppLocale = typeof supportedLocales[number]

export function resolveLocale(input?: string | null): AppLocale {
  const normalized = input?.toLowerCase()

  if (normalized?.startsWith('zh')) {
    return 'zh-CN'
  }

  return 'en-US'
}

export function isAppLocale(input: string | null): input is AppLocale {
  return supportedLocales.includes(input as AppLocale)
}

