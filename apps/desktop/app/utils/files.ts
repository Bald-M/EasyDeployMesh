export function formatBytes(bytes: number, locale: string): string {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return '0 B'
  }

  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const unitIndex = Math.min(
    Math.floor(Math.log(bytes) / Math.log(1024)),
    units.length - 1
  )
  const value = bytes / 1024 ** unitIndex

  return `${new Intl.NumberFormat(locale, {
    maximumFractionDigits: unitIndex === 0 ? 0 : 1
  }).format(value)} ${units[unitIndex]}`
}

export function compactHash(hash: string | null): string {
  if (!hash) {
    return '—'
  }

  return hash.length > 16
    ? `${hash.slice(0, 8)}…${hash.slice(-8)}`
    : hash
}
