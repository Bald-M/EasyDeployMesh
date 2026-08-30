export type ImageOperatingSystem =
  | 'windows-xp'
  | 'windows-7'
  | 'windows-8'
  | 'windows-10'
  | 'windows-11'
  | 'windows-server'
  | 'ubuntu'
  | 'unknown'

const WINDOWS_VERSION_PATTERNS: Array<[
  ImageOperatingSystem,
  RegExp
]> = [
  ['windows-11', /(?:windows|win)[\s._-]*11(?=$|[\\/\s._-])/i],
  ['windows-10', /(?:windows|win)[\s._-]*10(?=$|[\\/\s._-])/i],
  ['windows-8', /(?:windows|win)[\s._-]*8(?:[\s._-]*1)?(?=$|[\\/\s._-])/i],
  ['windows-7', /(?:windows|win)[\s._-]*7(?=$|[\\/\s._-])/i],
  ['windows-xp', /(?:windows|win)[\s._-]*xp(?=$|[\\/\s._-])/i],
  [
    'windows-server',
    /(?:windows[\s._-]*)?(?:server(?=$|[\s._-])|win(?:dows)?[\s._-]*(?:2003|2008|2012|2016|2019|2022|2025)(?=$|[\s._-]))/i
  ]
]

export function detectImageOperatingSystem(
  ...candidates: Array<string | null | undefined>
): ImageOperatingSystem {
  const searchableText = candidates.filter(Boolean).join(' ')

  if (/(?:^|[\\/\s._-])ubuntu(?=$|[\\/\s._-])/iu.test(searchableText)) {
    return 'ubuntu'
  }

  for (const [operatingSystem, pattern] of WINDOWS_VERSION_PATTERNS) {
    if (pattern.test(searchableText)) {
      return operatingSystem
    }
  }

  return 'unknown'
}
