// Run with `node --test .github/scripts/*.test.mjs`
import assert from 'node:assert/strict'
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

import { setVersion, versionOf, WORKSPACE_CRATES } from './set-version.mjs'

const repository = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
const FILES = ['Cargo.toml', 'Cargo.lock', 'package.json', join('src-tauri', 'tauri.conf.json')]

/** A copy of the four files of the repository to run the script on */
function copyOfRepository() {
  const root = mkdtempSync(join(tmpdir(), 'set-version-'))
  mkdirSync(join(root, 'src-tauri'))
  for (const file of FILES) {
    cpSync(join(repository, file), join(root, file))
  }
  return root
}

const read = (root, file) => readFileSync(join(root, file), 'utf8')

test('a tag with or without the v names the version', () => {
  assert.equal(versionOf('v1.2.3'), '1.2.3')
  assert.equal(versionOf('1.2.3'), '1.2.3')
  assert.equal(versionOf('v10.20.30'), '10.20.30')
  assert.equal(versionOf('v0.0.1'), '0.0.1')
  assert.equal(versionOf(' v1.2.3\n'), '1.2.3')
})

test('a pre-release is a version too', () => {
  for (const version of ['1.2.3-rc.1', '1.2.3-beta', '1.2.3-alpha.1.2', '1.0.0-0', '1.0.0-x-y.7']) {
    assert.equal(versionOf(`v${version}`), version)
  }
})

test('anything that is not a version is refused with the form that is expected', () => {
  const refused = [
    'main', 'release', '', undefined, 'v', 'v1', 'v1.2', 'v1.2.3.4', 'V1.2.3', 'vv1.2.3', 'version1.2.3',
    'v01.2.3', 'v1.02.3', 'v1.2.03', 'v1.2.3-', 'v1.2.3-rc..1', 'v1.2.3-rc.01', 'v1.2.3+build.5',
    'v1.2.3 rc1', 'v1.2.x', 'v-1.2.3', 'refs/tags/v1.2.3', 'v1.2.3;rm -rf', 'v1.2.3"',
  ]
  for (const tag of refused) {
    assert.throws(() => versionOf(tag), /vMAJOR\.MINOR\.PATCH/, `${JSON.stringify(tag)} was accepted`)
  }
})

test('the version lands in all four files and nothing else of them changes', () => {
  const root = copyOfRepository()
  try {
    const original = Object.fromEntries(FILES.map((file) => [file, read(root, file)]))
    assert.equal(setVersion(root, 'v7.8.9-rc.2'), '7.8.9-rc.2')

    for (const file of FILES) {
      const changed = read(root, file)
      assert.notEqual(changed, original[file], `${file} was not changed`)
      // the files differ in the version lines and in nothing else
      const lines = (text) => text.split('\n')
      const differing = lines(changed).filter((line, index) => line !== lines(original[file])[index])
      assert.ok(differing.every((line) => line.includes('7.8.9-rc.2')), `${file}: ${differing}`)
      assert.equal(lines(changed).length, lines(original[file]).length, file)
    }
    assert.match(read(root, 'Cargo.toml'), /\[workspace\.package\][^[]*\nversion = "7\.8\.9-rc\.2"/)
    assert.equal(JSON.parse(read(root, 'package.json')).version, '7.8.9-rc.2')
    assert.equal(JSON.parse(read(root, join('src-tauri', 'tauri.conf.json'))).version, '7.8.9-rc.2')
    for (const crate of WORKSPACE_CRATES) {
      assert.match(read(root, 'Cargo.lock'), new RegExp(`name = "${crate}"\\nversion = "7\\.8\\.9-rc\\.2"`))
    }
  } finally {
    rmSync(root, { recursive: true })
  }
})

test('only the workspace crates change in Cargo.lock, never a dependency that shares a number', () => {
  const root = copyOfRepository()
  try {
    const versionsOf = (lock) => [...lock.matchAll(/\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"/g)].map(([, name, version]) => `${name} ${version}`)
    const before = versionsOf(read(root, 'Cargo.lock'))
    setVersion(root, 'v1.0.0')
    // same version as the repository has, so nothing at all changes
    assert.deepEqual(versionsOf(read(root, 'Cargo.lock')), before)

    setVersion(root, 'v99.0.0')
    const after = versionsOf(read(root, 'Cargo.lock'))
    const changed = after.filter((entry, index) => entry !== before[index])
    assert.deepEqual(changed.sort(), WORKSPACE_CRATES.map((crate) => `${crate} 99.0.0`).sort())
  } finally {
    rmSync(root, { recursive: true })
  }
})

test('setting the same version twice changes nothing the second time', () => {
  const root = copyOfRepository()
  try {
    setVersion(root, 'v3.2.1')
    const once = FILES.map((file) => read(root, file))
    setVersion(root, '3.2.1')
    assert.deepEqual(FILES.map((file) => read(root, file)), once)
  } finally {
    rmSync(root, { recursive: true })
  }
})

test('a bad tag touches no file', () => {
  const root = copyOfRepository()
  try {
    const before = FILES.map((file) => read(root, file))
    assert.throws(() => setVersion(root, 'main'), /not a version/)
    assert.deepEqual(FILES.map((file) => read(root, file)), before)
  } finally {
    rmSync(root, { recursive: true })
  }
})

test('a file that changed its shape fails loudly, before anything is written', () => {
  const root = copyOfRepository()
  try {
    // the version of the workspace gone from Cargo.toml
    const cargoToml = read(root, 'Cargo.toml').replace(/^version = ".*"\n/m, '')
    writeFileSync(join(root, 'Cargo.toml'), cargoToml)
    const before = FILES.map((file) => read(root, file))
    assert.throws(() => setVersion(root, 'v2.0.0'), /\[workspace\.package\]/)
    assert.deepEqual(FILES.map((file) => read(root, file)), before)

    // a Cargo.lock without one of the workspace crates
    const lock = read(root, 'Cargo.lock').replace(/\[\[package\]\]\nname = "hitomi-server"\nversion = "[^"]*"/, '[[package]]\nname = "hitomi-renamed"\nversion = "1.0.0"')
    writeFileSync(join(root, 'Cargo.lock'), lock)
    writeFileSync(join(root, 'Cargo.toml'), read(repository, 'Cargo.toml'))
    assert.throws(() => setVersion(root, 'v2.0.0'), /hitomi-server/)
  } finally {
    rmSync(root, { recursive: true })
  }
})

test('the repository itself is in a shape the script understands, and agrees with itself', () => {
  const root = copyOfRepository()
  try {
    // every file is found and replaced exactly once, whichever version is in them
    assert.doesNotThrow(() => setVersion(root, 'v0.0.1'))
    const versions = new Set([
      JSON.parse(read(root, 'package.json')).version,
      JSON.parse(read(root, join('src-tauri', 'tauri.conf.json'))).version,
      read(root, 'Cargo.toml').match(/\[workspace\.package\][^[]*?\nversion = "([^"]+)"/)[1],
    ])
    assert.deepEqual([...versions], ['0.0.1'])
  } finally {
    rmSync(root, { recursive: true })
  }
})

test('files checked out with Windows line endings work too, and keep them', () => {
  const root = copyOfRepository()
  try {
    for (const file of FILES) {
      writeFileSync(join(root, file), read(root, file).replace(/\r?\n/g, '\r\n'))
    }
    setVersion(root, 'v7.8.9')
    for (const file of FILES) {
      const changed = read(root, file)
      assert.ok(changed.includes('7.8.9'), `${file} was not changed`)
      assert.equal(changed.replace(/\r\n/g, ''), changed.replace(/\r?\n/g, '').replace(/\r/g, ''), `${file} lost its line endings`)
      assert.ok(!/[^\r]\n/.test(changed), `${file} has a bare line feed`)
    }
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
