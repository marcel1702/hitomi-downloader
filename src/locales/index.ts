import deDE from './de-DE.json'
import enUS from './en-US.json'

const _locales = {
  'de-DE': deDE,
  'en-US': enUS,
}

// sort locales by key
export const locales = Object.fromEntries(
  Object.entries(_locales).sort(([keyA], [keyB]) => {
    return keyA.localeCompare(keyB)
  }),
) as typeof _locales

export type SupportedLocales = keyof typeof locales
export type MessageSchema = (typeof locales)['en-US']

/** The locale the app uses when nothing else matches */
export const DEFAULT_LOCALE: SupportedLocales = 'en-US'

/**
 * Pick the supported locale that fits `candidate` best
 *
 * The full tag wins, otherwise the language part alone, so a browser or a saved
 * config set to `de` or `de-AT` still lands on `de-DE`. Anything unknown falls back
 * to English, which also covers a config written by a version that shipped a locale
 * this one no longer has.
 */
export function resolveLocale(candidate: string | null | undefined): SupportedLocales {
  if (candidate === null || candidate === undefined) {
    return DEFAULT_LOCALE
  }

  if (candidate in locales) {
    return candidate as SupportedLocales
  }

  const language = candidate.split('-')[0].toLowerCase()
  const match = Object.keys(locales).find((key) => key.split('-')[0].toLowerCase() === language)

  return (match as SupportedLocales | undefined) ?? DEFAULT_LOCALE
}
