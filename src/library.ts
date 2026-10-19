// Sorting and searching the downloaded comics of the local tab
//
// The backend sends the whole library when the tab opens, newest download first, so both
// happen here. Kept free of Vue and the app, `library.test.mjs` runs it under Node.

import type { Comic } from './bindings.ts'

export const SORT_PRESETS = [
  'downloaded_newest',
  'downloaded_oldest',
  'date_newest',
  'date_oldest',
  'title_asc',
  'title_desc',
  'artist_asc',
  'pages_most',
  'pages_fewest',
] as const

export type SortPreset = (typeof SORT_PRESETS)[number]

/**
 * The time of a date as Hitomi writes it, `2024-03-01 10:00:00-05`, `undefined` for anything else
 *
 * `Date.parse` does not take that form everywhere, it is turned into ISO 8601 first.
 */
export function parseHitomiDate(date: string): number | undefined {
  const match = /^(\d{4}-\d{2}-\d{2})(?:[ T](\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?)(?:\s*(Z|[+-]\d{2})(?::?(\d{2}))?)?)?$/.exec(
    date.trim(),
  )
  if (match === null) return undefined
  const [, day, time = '00:00:00', zone, zoneMinutes = '00'] = match
  const offset = zone === undefined ? 'Z' : zone === 'Z' ? 'Z' : `${zone}:${zoneMinutes}`
  const ms = Date.parse(`${day}T${time}${offset}`)
  return Number.isNaN(ms) ? undefined : ms
}

/**
 * `comics` in the order of `preset`, a new array
 *
 * `comics` is in the order of the backend, newest download first. Titles and artists compare
 * in the language of the interface: case does not matter, `Ä` sorts with `A`, `Vol 2` before
 * `Vol 10`. Comics that are equal keep the order of the backend.
 */
export function sortComics(comics: Comic[], preset: SortPreset, locale: string): Comic[] {
  const collator = new Intl.Collator(locale, { sensitivity: 'base', numeric: true })
  const byTitle = (a: Comic, b: Comic) => collator.compare(a.title, b.title)
  // comics without a date go to the end, whichever way the dates are sorted
  const byDate = (direction: 1 | -1) => {
    const times = new Map(comics.map((comic) => [comic.id, parseHitomiDate(comic.date)]))
    return (a: Comic, b: Comic) => {
      const timeA = times.get(a.id)
      const timeB = times.get(b.id)
      if (timeA === undefined || timeB === undefined) return Number(timeA === undefined) - Number(timeB === undefined)
      return direction * (timeA - timeB)
    }
  }

  const sorted = [...comics]
  switch (preset) {
    case 'downloaded_newest':
      return sorted
    case 'downloaded_oldest':
      return sorted.reverse()
    case 'date_newest':
      return sorted.sort(byDate(-1))
    case 'date_oldest':
      return sorted.sort(byDate(1))
    case 'title_asc':
      return sorted.sort(byTitle)
    case 'title_desc':
      return sorted.sort((a, b) => byTitle(b, a))
    case 'artist_asc':
      // the first artist, then the title, comics without an artist at the end
      return sorted.sort((a, b) => {
        const artistA = a.artists[0]
        const artistB = b.artists[0]
        if (artistA === undefined || artistB === undefined) {
          return Number(artistA === undefined) - Number(artistB === undefined) || byTitle(a, b)
        }
        return collator.compare(artistA, artistB) || byTitle(a, b)
      })
    case 'pages_most':
      return sorted.sort((a, b) => b.files.length - a.files.length)
    case 'pages_fewest':
      return sorted.sort((a, b) => a.files.length - b.files.length)
  }
  return sorted
}

/** The prefixes of Hitomi that the search of the local tab knows */
const NAMESPACES = ['artist', 'group', 'series', 'character', 'language', 'type', 'female', 'male', 'tag'] as const

type Namespace = (typeof NAMESPACES)[number]

export type Term = {
  /** `-` in front, the comics that match are left out */
  negative: boolean
  /** `undefined` for a word that is looked for in the titles, the artists and the groups */
  namespace?: Namespace
  /** lower case, `_` turned into a space */
  value: string
}

/**
 * The words of a search, as the search tab takes them
 *
 * `artist:sakura_ito` is the artist `sakura ito`, `-language:english` leaves out the comics in
 * English. A prefix Hitomi does not know is part of the word.
 */
export function parseQuery(query: string): Term[] {
  const terms: Term[] = []
  for (const word of query.trim().toLowerCase().split(/\s+/)) {
    const negative = word.startsWith('-')
    const rest = (negative ? word.slice(1) : word).replace(/_/g, ' ')
    const colon = rest.indexOf(':')
    const prefix = colon > 0 ? rest.slice(0, colon) : ''
    const namespace = NAMESPACES.find((known) => known === prefix)
    const value = namespace === undefined ? rest : rest.slice(colon + 1)
    // a lone `-`, or `artist:` without a name, asks for nothing
    if (value.trim() === '') continue
    terms.push({ negative, namespace, value: value.trim() })
  }
  return terms
}

const same = (a: string, b: string) => a.toLowerCase() === b

function matches(comic: Comic, term: Term): boolean {
  const { namespace, value } = term
  switch (namespace) {
    case undefined:
      return [comic.title, comic.japaneseTitle, ...comic.artists, ...comic.groups].some((text) =>
        text.toLowerCase().includes(value),
      )
    case 'artist':
      return comic.artists.some((artist) => same(artist, value))
    case 'group':
      return comic.groups.some((group) => same(group, value))
    case 'series':
      return comic.parodys.some((series) => same(series, value))
    case 'character':
      return comic.characters.some((character) => same(character, value))
    case 'language':
      return same(comic.language, value)
    case 'type':
      return same(comic.type, value)
    // a tag is female, male or neither, as the prefix of the search tab says
    case 'female':
      return comic.tags.some(({ tag, female }) => female !== 0 && same(tag, value))
    case 'male':
      return comic.tags.some(({ tag, male }) => male !== 0 && same(tag, value))
    case 'tag':
      return comic.tags.some(({ tag, female, male }) => female === 0 && male === 0 && same(tag, value))
  }
}

/**
 * The comics that match `query`, in their order
 *
 * Only digits is the comic with exactly that ID. Otherwise every word has to match, a word
 * with `-` in front must not.
 */
export function filterComics(comics: Comic[], query: string): Comic[] {
  const trimmed = query.trim()
  if (trimmed === '') return comics
  if (/^\d+$/.test(trimmed)) return comics.filter((comic) => comic.id === Number(trimmed))
  const terms = parseQuery(trimmed)
  return comics.filter((comic) => terms.every((term) => matches(comic, term) !== term.negative))
}
