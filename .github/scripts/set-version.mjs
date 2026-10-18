// Write the version of a release into the files that hold it, before it is built
//
// Usage: node .github/scripts/set-version.mjs <tag or version> [repository root]
//
// `v1.2.3` and `1.2.3` both set the version 1.2.3, a pre-release as in `v1.2.3-rc.1` is
// allowed. Nothing else is accepted, and then no file is touched.
//
// The version lives in four places, which have to agree (the unit test `versions_are_in_step`
// checks the first three): the workspace version in `Cargo.toml`, which the server and the
// `ComicInfo.xml` of every download report, `src-tauri/tauri.conf.json`, which the desktop
// About dialog reports, `package.json`, and `Cargo.lock`, which repeats the version of the
// workspace crates and would make `cargo build --locked` fail if it did not follow.
// Nothing is committed, a release is built from the tag and the files stay as they are.
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

/** The workspace crates, whose entries in `Cargo.lock` carry the version */
export const WORKSPACE_CRATES = ['hitomi-core', 'hitomi-downloader', 'hitomi-server']

// MAJOR.MINOR.PATCH without leading zeros, then an optional pre-release of dot separated
// identifiers (semver.org). Build metadata (`+...`) is not accepted, an installer name could
// not carry it.
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-(0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(\.(0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?$/

/** The version a tag or a version string names, or an error that says what is wrong */
export function versionOf(tag) {
  const version = String(tag ?? '').trim().replace(/^v/, '')
  if (!VERSION.test(version)) {
    throw new Error(
      `\`${tag}\` is not a version. A release is tagged vMAJOR.MINOR.PATCH, optionally with a pre-release such as -rc.1, for example v1.2.3 or v1.2.3-rc.1`,
    )
  }
  return version
}

/** `text` with the one match of `pattern` replaced, failing if the file has a different shape */
function replaceOnce(text, pattern, replacement, what) {
  const matches = text.match(new RegExp(pattern.source, pattern.flags.includes('g') ? pattern.flags : `${pattern.flags}g`))
  if (matches?.length !== 1) {
    throw new Error(`Expected exactly one ${what}, found ${matches?.length ?? 0}. The file changed its shape, update .github/scripts/set-version.mjs`)
  }
  return text.replace(pattern, replacement)
}

/** Set `version` in every file under `root`, all or none */
export function setVersion(root, tag) {
  const version = versionOf(tag)
  const files = {
    cargoToml: join(root, 'Cargo.toml'),
    cargoLock: join(root, 'Cargo.lock'),
    tauriConf: join(root, 'src-tauri', 'tauri.conf.json'),
    packageJson: join(root, 'package.json'),
  }
  const before = Object.fromEntries(Object.entries(files).map(([name, path]) => [name, readFileSync(path, 'utf8')]))
  const after = {}

  // the version of `[workspace.package]`, and not a dependency's
  const section = before.cargoToml.indexOf('[workspace.package]')
  if (section === -1) {
    throw new Error('Cargo.toml has no [workspace.package], update .github/scripts/set-version.mjs')
  }
  const sectionEnd = before.cargoToml.indexOf('\n[', section + 1)
  const head = before.cargoToml.slice(0, section)
  const body = before.cargoToml.slice(section, sectionEnd === -1 ? undefined : sectionEnd)
  const tail = sectionEnd === -1 ? '' : before.cargoToml.slice(sectionEnd)
  after.cargoToml = head + replaceOnce(body, /^version = "[^"]*"/m, `version = "${version}"`, 'version in [workspace.package]') + tail

  after.cargoLock = WORKSPACE_CRATES.reduce(
    (lock, crate) => replaceOnce(lock, new RegExp(`(\\[\\[package\\]\\]\\r?\\nname = "${crate}"\\r?\\nversion = )"[^"]*"`), `$1"${version}"`, `Cargo.lock entry of ${crate}`),
    before.cargoLock,
  )
  // the top level key, which is the only one indented by two spaces
  after.tauriConf = replaceOnce(before.tauriConf, /^ {2}"version": "[^"]*"/m, `  "version": "${version}"`, 'version in tauri.conf.json')
  after.packageJson = replaceOnce(before.packageJson, /^ {2}"version": "[^"]*"/m, `  "version": "${version}"`, 'version in package.json')

  // nothing is written before everything was replaced
  for (const [name, path] of Object.entries(files)) {
    if (after[name] !== before[name]) {
      writeFileSync(path, after[name])
    }
  }
  return version
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const [tag, root = process.cwd()] = process.argv.slice(2)
  try {
    const version = setVersion(root, tag)
    console.log(`Version set to ${version} (from \`${tag}\`) in Cargo.toml, Cargo.lock, src-tauri/tauri.conf.json and package.json`)
  } catch (error) {
    console.error(`::error::${error.message}`)
    process.exit(1)
  }
}
