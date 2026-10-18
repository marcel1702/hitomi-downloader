<p align="center">
    <img src="https://github.com/user-attachments/assets/efd0470a-f5cb-4c1d-a0c3-3f5c39113933" style="align-self: center"/>
</p>

# 📚 Hitomi Downloader

A GUI-based multi-threaded downloader for [hitomi.la](https://hitomi.la), built with [Tauri](https://v2.tauri.app/start/).

Comics are downloaded straight into a `.cbz` archive with a `ComicInfo.xml` that follows the
ComicRack v2.1 schema, so they drop into Kavita, Komga or any other comic reader without a
conversion step.

## 📥 Download

Pre-compiled packages are on the [Releases page](https://github.com/marcel1702/hitomi-downloader/releases).
Download and run, no installation of a runtime required.

**Enjoying this project? A GitHub Star⭐ is always welcome 🙏**

## ✨ Features

| Feature | Description |
| --- | --- |
| 🖼️ GUI | Lightweight desktop app, no browser or command line needed. |
| ⚡ Multi-threaded downloading | Several comics and several pages at a time. |
| 📦 CBZ archives | A download lands as a single `.cbz` next to its `metadata.json` and cover, not as a loose folder of images. |
| 🏷️ ComicInfo.xml | Validated against the ComicRack v2.1 schema, with title, series, artists, date, language, characters, circle and tags. |
| 📖 Built-in reader | Read a downloaded comic straight from its archive, in page or scroll mode. |
| 📂 Export | One-click export to PDF or CBZ into a separate export directory. |
| 🌐 English and German | The interface ships both languages and switches at runtime. |
| 🗂️ Custom folder structure | Freely configurable naming from fields like type, artist, title, language and id. |

## 🖥️ Screenshots

![image](https://github.com/user-attachments/assets/fd93fd2f-db16-43b6-86cf-aa643eb572c8)
![image](https://github.com/user-attachments/assets/81a859f2-2a06-4eca-b45f-4f6555cc62c0)

## 📖 How to use

1. Search for keywords in the `Search` tab, or paste a comic id or gallery link.
2. Hit `Download` on the comic card, or open the `Comic` tab through the cover for more detail
   and download from there.
3. Watch the progress under `Downloading` on the right; finished comics move to `Completed`.
4. Once finished, the comic shows up under `Local`, where `Open Folder` takes you to the
   archive. Click the cover to open it in the `Comic` tab, where `Read` opens it in the
   built-in reader.

Exporting to PDF or CBZ is also done from the `Local` tab, through the buttons on each comic,
the right-click menu, or `Export All` for the whole library.

📹 The video below walks through the whole flow. **It is safe for work.**

https://github.com/user-attachments/assets/d2d0e577-c074-41ca-996f-445d52e2cce5

## 📄 What a download looks like

```
<download directory>/
└── <name from your folder format>/
    ├── <name from your folder format>.cbz   the pages plus ComicInfo.xml
    ├── metadata.json                        what the library view is built from
    └── cover.webp                           thumbnail for the library view
```

Downloads made by older versions, which stored loose images, keep working: the reader and
both exports fall back to the folder when no archive is present.

## 📚 Using the download directory with Kavita

The download directory can be the folder of a [Kavita](https://www.kavitareader.com) library, at
any time, also while a download runs. Kavita scans for the **file types** you choose for the
library, so set them as follows:

- **File types: `Archive` only.** The `.cbz` is the only file Kavita is meant to find, and it
  carries everything it needs: the pages and the `ComicInfo.xml` with title, series, artists,
  tags and language. `metadata.json` and `cover.webp` are for this app, Kavita skips them.
- Do not add `Image` to the file types. Kavita counts `.webp` and `.avif` as images, so it
  would list the pages of a download that is still running as a series of their own.

**Why an unfinished download is never listed.** Kavita decides by the last extension of a
file name. The archive is therefore written as `<name>.cbz.tmp`, which Kavita reads as `.tmp`
and ignores. Only when it is complete and written to the disk it is renamed to `<name>.cbz`,
which is a single step, and only after that are the pages it was made from deleted. A comic
that is downloaded again replaces its archive in the same way, Kavita sees the old or the new
one and never half of it.

**Where the pages are while a comic downloads**
- **Desktop app:** in a `.downloading-<name>` folder next to the comic. They are images, not
  archives, so Kavita ignores them with the file types above. If you do have `Image` enabled,
  add the exclude pattern `.downloading-*/` in the library settings of Kavita. Kavita matches
  the pattern against directory names written as `name/`.
- **Web version:** outside of the library, in `HITOMI_TEMP_DIR` (by default `downloading` in
  the config directory). The library only ever holds the `.cbz.tmp` and the finished `.cbz`,
  whatever the file types of Kavita are, and a network share gets the archive written once.
  Plan for 10 to 100 MB of space per running comic there, twice that with the default of two
  comics at once. The server refuses to start if that directory lies inside the library.

A download that is cancelled or interrupted leaves its pages behind, and a crash may leave a
`.cbz.tmp`. Both are harmless, Kavita ignores them, and they are replaced or removed when the
comic is downloaded again.

## ⚠️ About antivirus false positives

Unsigned binaries from small projects get flagged regularly, since a code signing certificate
costs money that a hobby project does not have. Two ways around it:

1. Build it yourself with the steps below.
2. Check the build workflow under `.github/workflows/release.yml` — every release is built by
   GitHub Actions from the source in this repository, so you can follow exactly what went in.

## 🛠️ How to build

#### 📋 Prerequisites

- [Rust](https://www.rust-lang.org/tools/install)
- [Node](https://nodejs.org/en)
- [pnpm](https://pnpm.io/installation)

On Linux the Tauri system libraries are needed as well:

```
sudo apt-get install libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libsoup-3.0-dev
```

#### 📝 Steps

```
git clone https://github.com/marcel1702/hitomi-downloader.git
cd hitomi-downloader
pnpm install
pnpm tauri build
```

For a development build with hot reload, use `pnpm tauri dev` instead.

#### 🏷️ Making a release

Push a tag named `vMAJOR.MINOR.PATCH`, for example `v1.2.3`, or `v1.2.3-rc.1` for a
pre-release. The release workflow takes the version from the tag and writes it into
`Cargo.toml`, `Cargo.lock`, `src-tauri/tauri.conf.json` and `package.json` before it builds, so
the About dialogs, the web version, the `ComicInfo.xml` of the downloads and the names of the
installers all say the same. **There is no version to edit before tagging.** A tag that is not
a version stops the workflow before anything is built. The version in the repository is only
what a build on your own machine reports.

`node .github/scripts/set-version.mjs v1.2.3` does the same by hand, should you want to try it,
and `node --test .github/scripts/*.test.mjs` tests it.

## 🌐 Adding a new language

Translations are welcome. The interface ships English and German, and a further language is
three steps:

1. Copy `src/locales/en-US.json` to `src/locales/<tag>.json` and translate the values. Keep
   every key — a missing key silently falls back to English.
2. Register the file in `src/locales/index.ts`.
3. Add the language tag to `SUPPORTED_LANGUAGES` in `src-tauri/src/config.rs`, so a saved
   config naming it is not reset to English on startup.

Tags and comic types are Hitomi's own vocabulary and stay untranslated on purpose.

## 🤝 Contributing

Open an `issue` or a `discussion` before starting on a new feature, so no effort goes to
waste. Everything else is welcome as a pull request against `main`:

1. 🔧 Improvements to existing features
2. 🐛 Bug fixes
3. 🌐 New languages
4. ⚡ Lighter implementations of what is already there
5. 📝 Documentation
6. ⬆️ Dependency updates

## ⚠️ Disclaimer

- This tool is meant for learning, research and personal use. Anyone using it does so at their
  own risk.
- The authors are not responsible for any loss, legal dispute or other consequence arising
  from the use of this tool.
- The authors are not responsible for what users do with this tool, including but not limited
  to anything that breaks the law or the rights of a third party.

## 🙏 Credits

This project stands on other people's work:

- [lanyeeee/hitomi-downloader](https://github.com/lanyeeee/hitomi-downloader) — the original
  project this is ultimately derived from.
- [warpdotsys/hitomi-downloader](https://github.com/warpdotsys/hitomi-downloader) — the fork
  this repository builds on.
- [Pupil](https://github.com/tom5079/Pupil) — reference for talking to Hitomi.

## 📜 License

Released under the [MIT License](https://github.com/marcel1702/hitomi-downloader/blob/main/LICENSE).

Copyright © 2026 [marcel1702](https://github.com/marcel1702), and the upstream authors whose
copyright notice the license retains.

## 💬 Anything else

Run into a problem or missing a feature? Open an `issue` or a `discussion`.
