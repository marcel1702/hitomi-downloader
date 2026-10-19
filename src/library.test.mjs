// Tests of the sorting and the search of the local tab, `node --test src/*.test.mjs`
//
// Node runs `library.ts` as it is, it strips the types itself.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { filterComics, parseHitomiDate, parseQuery, sortComics } from './library.ts'

const tag = (name, { female = 0, male = 0 } = {}) => ({ tag: name, female, male })

function comic(id, fields = {}) {
  return {
    id,
    title: `Comic ${id}`,
    japaneseTitle: '',
    language: 'english',
    languageLocalname: 'English',
    type: 'doujinshi',
    date: '2024-01-01 00:00:00-05',
    artists: [],
    groups: [],
    parodys: [],
    tags: [],
    related: [],
    languages: [],
    characters: [],
    sceneIndexes: [],
    files: [],
    coverUrl: '',
    ...fields,
  }
}

const pages = (count) => Array.from({ length: count }, () => ({}))
const ids = (comics) => comics.map((c) => c.id)

// in the order of the backend, newest download first
const library = [
  comic(1, { title: 'Vol 10', artists: ['bob'], date: '2023-05-01 12:00:00-05', files: pages(5) }),
  comic(2, { title: 'vol 2', artists: [], date: '2025-01-01 08:00:00+09', files: pages(30) }),
  comic(3, { title: 'Ärger', artists: ['Anna'], date: 'unknown', files: pages(5) }),
  comic(4, { title: 'apple', artists: ['anna'], date: '2024-12-31 23:00:00-05', files: pages(12) }),
  comic(5, { title: 'Zebra', artists: ['Carl', 'anna'], date: '2020-01-01 00:00:00-05', files: pages(1) }),
]

test('a date as Hitomi writes it', () => {
  assert.equal(parseHitomiDate('2024-03-01 10:00:00-05'), Date.UTC(2024, 2, 1, 15))
  assert.equal(parseHitomiDate('2024-03-01 10:00:00+09:30'), Date.UTC(2024, 2, 1, 0, 30))
  assert.equal(parseHitomiDate('2024-03-01 10:00:00'), Date.UTC(2024, 2, 1, 10))
  assert.equal(parseHitomiDate('2024-03-01'), Date.UTC(2024, 2, 1))
  assert.equal(parseHitomiDate(''), undefined)
  assert.equal(parseHitomiDate('yesterday'), undefined)
})

test('recently downloaded is the order of the backend, first downloaded the reverse', () => {
  assert.deepEqual(ids(sortComics(library, 'downloaded_newest', 'en-US')), [1, 2, 3, 4, 5])
  assert.deepEqual(ids(sortComics(library, 'downloaded_oldest', 'en-US')), [5, 4, 3, 2, 1])
  // a new array, the list of the tab stays as it is
  assert.notEqual(sortComics(library, 'downloaded_newest', 'en-US'), library)
  assert.deepEqual(ids(library), [1, 2, 3, 4, 5])
})

test('dates sort by the moment, with their time zone, and an unknown date goes last', () => {
  // 2025-01-01 08:00+09 is 2024-12-31 23:00 UTC, before 2024-12-31 23:00-05
  assert.deepEqual(ids(sortComics(library, 'date_newest', 'en-US')), [4, 2, 1, 5, 3])
  assert.deepEqual(ids(sortComics(library, 'date_oldest', 'en-US')), [5, 1, 2, 4, 3])
})

test('titles ignore case and accents and count numbers as numbers', () => {
  assert.deepEqual(ids(sortComics(library, 'title_asc', 'de-DE')), [4, 3, 2, 1, 5])
  assert.deepEqual(ids(sortComics(library, 'title_desc', 'de-DE')), [5, 1, 2, 3, 4])
})

test('artists by the first one, then the title, comics without an artist last', () => {
  // Anna and anna are the same artist, then by title: apple before Ärger
  assert.deepEqual(ids(sortComics(library, 'artist_asc', 'en-US')), [4, 3, 1, 5, 2])
})

test('pages, with equal ones in the order of the backend', () => {
  assert.deepEqual(ids(sortComics(library, 'pages_most', 'en-US')), [2, 4, 1, 3, 5])
  assert.deepEqual(ids(sortComics(library, 'pages_fewest', 'en-US')), [5, 1, 3, 4, 2])
})

test('the words of a search', () => {
  assert.deepEqual(parseQuery('  Artist:Sakura_Ito  -language:english foo:bar - tag: '), [
    { negative: false, namespace: 'artist', value: 'sakura ito' },
    { negative: true, namespace: 'language', value: 'english' },
    { negative: false, namespace: undefined, value: 'foo:bar' },
  ])
})

const shelf = [
  comic(2829145, {
    title: 'Alpha Story Vol 2',
    japaneseTitle: 'アルファ',
    artists: ['sakura ito'],
    groups: ['team red'],
    parodys: ['original'],
    characters: ['alice'],
    tags: [tag('big breasts', { female: 1 }), tag('glasses')],
  }),
  comic(28291, { title: 'Beta', artists: ['bob'], language: 'japanese', type: 'manga', tags: [tag('glasses', { female: 1 })] }),
  comic(100, { title: 'Gamma', groups: ['blue team'], language: 'german', tags: [tag('muscle', { male: 1 })] }),
]

test('only digits is exactly that ID, no prefix', () => {
  assert.deepEqual(ids(filterComics(shelf, '2829145')), [2829145])
  assert.deepEqual(ids(filterComics(shelf, ' 28291 ')), [28291])
  assert.deepEqual(ids(filterComics(shelf, '2829')), [])
})

test('text is looked for in the titles, the artists and the groups, every word', () => {
  assert.deepEqual(ids(filterComics(shelf, 'ALPHA')), [2829145])
  assert.deepEqual(ids(filterComics(shelf, 'アルファ')), [2829145])
  assert.deepEqual(ids(filterComics(shelf, 'team')), [2829145, 100])
  assert.deepEqual(ids(filterComics(shelf, 'team blue')), [100])
  assert.deepEqual(ids(filterComics(shelf, 'sakura_ito')), [2829145])
  // tags are no text, they need their prefix
  assert.deepEqual(ids(filterComics(shelf, 'glasses')), [])
})

test('the prefixes of Hitomi match whole names', () => {
  assert.deepEqual(ids(filterComics(shelf, 'artist:sakura_ito')), [2829145])
  assert.deepEqual(ids(filterComics(shelf, 'artist:sakura')), [])
  assert.deepEqual(ids(filterComics(shelf, 'group:team_red')), [2829145])
  assert.deepEqual(ids(filterComics(shelf, 'series:original')), [2829145])
  assert.deepEqual(ids(filterComics(shelf, 'character:alice')), [2829145])
  assert.deepEqual(ids(filterComics(shelf, 'language:japanese')), [28291])
  assert.deepEqual(ids(filterComics(shelf, 'type:manga')), [28291])
  assert.deepEqual(ids(filterComics(shelf, 'female:big_breasts')), [2829145])
  assert.deepEqual(ids(filterComics(shelf, 'female:glasses')), [28291])
  assert.deepEqual(ids(filterComics(shelf, 'tag:glasses')), [2829145])
  assert.deepEqual(ids(filterComics(shelf, 'male:muscle')), [100])
  assert.deepEqual(ids(filterComics(shelf, 'Tag:Glasses')), [2829145])
})

test('a word with - leaves out what it matches', () => {
  assert.deepEqual(ids(filterComics(shelf, '-language:english')), [28291, 100])
  assert.deepEqual(ids(filterComics(shelf, '-tag:glasses -male:muscle')), [28291])
  assert.deepEqual(ids(filterComics(shelf, '-beta')), [2829145, 100])
  assert.deepEqual(ids(filterComics(shelf, 'team -group:team_red')), [100])
})

test('an empty search, or one that asks for nothing, is the whole library', () => {
  assert.equal(filterComics(shelf, ''), shelf)
  assert.equal(filterComics(shelf, '   '), shelf)
  assert.deepEqual(ids(filterComics(shelf, '- artist:')), ids(shelf))
})
