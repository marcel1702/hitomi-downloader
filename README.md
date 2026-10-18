<p align="center">
    <img src="https://github.com/user-attachments/assets/efd0470a-f5cb-4c1d-a0c3-3f5c39113933" style="align-self: center"/>
</p>

# 📚 Hitomi Downloader

A multi-threaded downloader for [hitomi.la](https://hitomi.la), as a desktop app built with
[Tauri](https://v2.tauri.app/start/) and as a [web version](#-web-version-docker) for a server, which runs in
Docker behind a login.

Comics are downloaded straight into a `.cbz` archive with a `ComicInfo.xml` that follows the
ComicRack v2.1 schema, so they open in any comic reader or comic server without a conversion
step.

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
| 📖 Built-in reader | Read a downloaded comic straight from its archive, in page or scroll mode, with thumbnails, the mouse wheel and a slideshow. |
| 📂 Export | One-click export to PDF or CBZ into a separate export directory. |
| ☑️ Selection and delete | Tick several comics to download, export or delete them together, in the search and the local tab. |
| 🌐 English and German | The interface ships both languages and switches at runtime. |
| 🗂️ Custom folder structure | Freely configurable naming from fields like type, artist, title, language and id. |
| 🐳 Web version | The same interface in the browser, as a Docker image for a server behind a login: [one place](#-web-version-docker) for the downloads, on a folder of the server. It keeps its download queue across restarts and can [follow searches](#following-searches), downloading new galleries by itself. |

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

In the reader the mouse wheel turns the pages (`M` switches to scrolling, `F` fits the width, `T`
shows thumbnails), and `S` starts or stops a slideshow that turns a page every 1 to 60 seconds, as
set next to the play button. It stops at the last page and as soon as you turn a page yourself.

Exporting to PDF or CBZ is also done from the `Local` tab, through the buttons on each comic,
the right-click menu, or `Export All` for the whole library. Tick the checkboxes of several comics
(the selection stays when you change the page, `Select All` takes every page) and `Export All`
becomes `Export Selection`. The `Search` tab works the same way: `Download All` below the search
fields becomes `Download Selection`.

`Delete` on a comic card, in the `Comic` tab, or `Delete Selection` for ticked comics removes the
comic from the library, in the desktop app and in the browser alike. It always asks first, and
it only deletes inside the download directory: exported files stay, and so does everything that
is not a downloaded comic. A comic that is still downloading can not be deleted.

📹 The video below walks through the whole flow. **It is safe for work.**

https://github.com/user-attachments/assets/d2d0e577-c074-41ca-996f-445d52e2cce5

## 🐳 Web version (Docker)

Besides the desktop app there is a web version for a server: the same interface in the browser,
one place to manage the downloads from any device, and the comics in a folder of the server.
It is a Docker image for `amd64` and `arm64`, `ghcr.io/marcel1702/hitomi-downloader`. It does not
contain the desktop app, there is no GTK or WebKit in it.

### Quick start

1. Make the hash of your password: `docker run --rm -it ghcr.io/marcel1702/hitomi-downloader hash-password`
2. Copy `.env.example` to `.env` next to [`docker-compose.yml`](docker-compose.yml) and fill it in: the
   login, the hash, and `LIBRARY_PATH`, the folder on the Docker host where the comics go. Keep the single
   quotes around the hash, they stop Docker Compose from reading its `$` as variables. The optional
   settings of the [table below](#settings) can be made in the same file.
3. `docker compose up -d`, then open `http://<server>:8080` and log in.

The image is as visible as this repository. While that is private, `docker login ghcr.io` with a token
that may read packages comes first.

### Settings

All of them are environment variables. The login has no default: **the server does not start without one**.

| Variable | Meaning |
|---|---|
| `HITOMI_USERNAME`, `HITOMI_PASSWORD_HASH` | The login. `HITOMI_PASSWORD` (plain) works too, less safe |
| `HITOMI_LIBRARY_DIR` | Where the downloads go, a directory that is mounted into the container. Default `/data/library`, any path will do |
| `HITOMI_EXPORT_DIR` | Where "Export to server" writes, default `/data/export`. Not inside the library |
| `HITOMI_CONFIG_DIR` | `config.json`, the logins, the logs, the download queue (`queue.json`), the followed searches (`subscriptions.json`) and the pages of running downloads, default `/config` |
| `PUID`, `PGID`, `UMASK` | The user and group the files belong to (default `1000:1000`), and their rights (default `022`) |
| `TZ` | Time zone of the logs |
| `HITOMI_HOST`, `HITOMI_PORT` | The address and port the server listens on, default `127.0.0.1:8080`. The image listens on `0.0.0.0:8080`, and `HITOMI_PORT` in the `.env` of the compose file is the port on the Docker host |
| `HITOMI_WEB_DIR` | The built frontend, default `dist-web`, `/app/web` in the image |
| `HITOMI_PUBLIC_URL` | The address you reach it under, behind a reverse proxy. `https://` makes the login cookie `Secure`, see [Login, https and the internet](#login-https-and-the-internet) |
| `HITOMI_TRUST_FORWARDED_FOR` | Behind a reverse proxy: take the client address from `X-Forwarded-For` and https from `X-Forwarded-Proto`. Only with a proxy that sets both, see below |
| `HITOMI_SESSION_DAYS` | How long a login lasts, default 30 days |
| `HITOMI_TEMP_DIR` | Where the pages of running downloads are, default `downloading` in the config directory |
| `HITOMI_FOLLOW_INTERVAL_HOURS` | How often the followed searches are checked for new galleries, default `6`, from `1` to `720`, `0` for no automatic checks |
| `HITOMI_REQUIRE_MOUNT` | Refuse to start if a data directory is not a mount. The image sets it to `true` |
| `HITOMI_MAX_NAME_BYTES` | A lower limit for file names, between 100 and 255 |

### Where the library can live

The library is only a directory for the container, it can be a folder of the Docker host, a named volume,
or a folder that is itself an NFS or SMB share mounted on the host. No path is a given, mount it where you
like and tell the server with `HITOMI_LIBRARY_DIR`. The comics hold no path, so the same folder can be called
one thing in another container and another here, and be moved to another mount point later.

- **Owner and rights.** The files belong to `PUID:PGID`. On an NFS share the numeric ids count, and its
  settings (root squash, a user it maps everyone to) decide who may write. `UMASK` decides whether other users
  can read what is written. **The library is never `chown`ed**, only `/config` is handed to the user.
- **Named volumes.** `/data/library` and `/data/export` are open to everyone in the image (`0777`). Docker copies
  that into a new named volume, so any `PUID` can write to it without a `chown`. Only the root folder of the
  volume is open: what the server writes in it has the rights of `UMASK`. A bind mount or a share keeps the
  rights of its folder on the host, the `0777` of the image does not apply to it. That is why it stays: in the
  container only the server runs, and on the host the volumes lie below `/var/lib/docker/volumes`, which only
  root can enter.
- **It checks at the start.** A directory the user can not write, with the user, its owner and what to do about
  it in the message, and a directory where nothing is mounted, which would lose its files with the container.
- **A missing mount.** With the short form of a bind mount Docker makes an empty folder on the host if the source
  is missing, and the downloads would go to its local disk without anyone noticing. Use the long form with
  `create_host_path: false`, as the compose file does, so that the container refuses to start without the mount.
- **Short file names.** The server finds out how long a file name may be in the library and shortens long
  titles to fit. An encrypted shared folder of a Synology NAS allows about 143 bytes and not 255. Set
  `HITOMI_MAX_NAME_BYTES` if a share lies about its limit.

### Login, https and the internet

Every page needs a login, there is no way around it. The session lasts 30 days and survives a restart,
five failed logins within 15 minutes lock the address out for 15 minutes. It still should not face the internet
without https: put a reverse proxy in front of it, set `HITOMI_PUBLIC_URL` to the address, and
`HITOMI_TRUST_FORWARDED_FOR=true`.

What the two change behind a reverse proxy:

| | Without them | With them |
|---|---|---|
| The login cookie | Not `Secure`, the browser also sends it over plain http | `Secure`: always with an `https://` `HITOMI_PUBLIC_URL`, and with `HITOMI_TRUST_FORWARDED_FOR` whenever the proxy sends `X-Forwarded-Proto: https` |
| The lockout | Counts the address of the proxy: five failed logins of anyone lock **everyone** out for 15 minutes | Counts the address of each client, from the last entry of `X-Forwarded-For` |

Set `HITOMI_TRUST_FORWARDED_FOR` only when every request comes through a proxy that sets these headers, and the
port of the server can not be reached otherwise: anyone who reaches the server directly could pick an address
and claim https with them.

### Downloading and exporting

In the browser a comic is saved on the device you sit at with **Download PDF** and **Download CBZ**, under its
real name. **Export All** and the entries "to server" of the context menu write into the export directory of
the server instead.

The download queue survives a restart or an update of the container. What waits, runs or is paused comes
back at the next start, and a download continues with the pages that are missing. A failed download stays in
the list for a retry. The queue is the file `queue.json` in the config directory: if it can not be read, it is
moved to `queue.json.corrupt` and the queue starts empty.

### Following searches

The server can look for new galleries by itself. Search for an artist, a group or a series, for example
`artist:beater language:english`, and press **Follow this search**. The dialog shows how many galleries match
and how many of them are in the library already, and asks how to start: **Only new galleries from now on**, or
**Download everything** (up to 1000 galleries, a search with more only follows from now on). The tab
**Following** lists what is followed, with the last check, the new galleries it found and its state, and has
the buttons to check now, pause and delete.

Every `HITOMI_FOLLOW_INTERVAL_HOURS` (6 by default) each followed search is run and the galleries it has not
dealt with yet are downloaded, the oldest first. A search is one index download, only new galleries cost a
request each, and the checks run one after the other. Galleries that are in the library, or that are being
downloaded already, are not downloaded twice. **A comic you delete from the library does not come back**: a
followed search remembers what it has dealt with. A check that fails shows the error on its card and is tried
again after half an hour. The followed searches are the file `subscriptions.json` in the config directory.

### Versions and updates

An image is tagged with its version: `1.2.3`, `1.2`, `1`, and `latest` for the newest release. A pre-release
only has its own tag. `docker compose pull && docker compose up -d` updates. To build the image yourself,
`docker build -t hitomi-downloader .` in this directory.

## 📄 What a download looks like

```
<download directory>/
└── <name from your folder format>/
    ├── <name from your folder format>.cbz   the pages plus ComicInfo.xml
    ├── metadata.json                        what the library view is built from
    └── cover.webp                           thumbnail for the library view
```

The folder format is set in the settings. The default is `{artists}/{title} ({id}) - {language}`, which
puts a comic in the folder of its artists, for example `mameroku/Soushi Souai. (2829145) - german`. The
id keeps two galleries with the same title apart. A comic without artists goes straight into the download
directory. A format you saved earlier is kept.

Downloads made by older versions, which stored loose images, keep working: the reader and
both exports fall back to the folder when no archive is present.

## 🛡️ Why an unfinished download is never in the library

Anything that scans the download directory decides by the last extension of a file name. The
archive is therefore written as `<name>.cbz.tmp` and only when it is complete and written to
the disk it is renamed to `<name>.cbz`, which is a single step. The pages it was made from are
deleted after that. A comic that is downloaded again replaces its archive the same way, so a
scanner sees the old or the new archive and never half of one.

**Where the pages are while a comic downloads**
- **Desktop app:** in a `.downloading-<name>` folder next to the comic.
- **Web version:** outside of the library, in `HITOMI_TEMP_DIR` (by default `downloading` in
  the config directory). The library only ever holds the `.cbz.tmp` and the finished `.cbz`,
  and a network share gets the archive written once. Plan for 10 to 100 MB of space per
  running comic there, twice that with the default of two comics at once. The server refuses
  to start if that directory lies inside the library.

A download that is cancelled or interrupted leaves its pages behind, and a crash may leave a
`.cbz.tmp`. Both are replaced or removed when the comic is downloaded again.

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

The web version needs no Tauri libraries. Build the frontend and run the server:

```
pnpm install
pnpm build:web
HITOMI_USERNAME=admin HITOMI_PASSWORD=change-me cargo run --release -p hitomi-server
```

It listens on `http://127.0.0.1:8080` and reads the frontend from `dist-web`, the settings are the
[environment variables above](#settings).

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
3. Add the language tag to `SUPPORTED_LANGUAGES` in `crates/core/src/config.rs`, so a saved
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

### Issues and labels

[Issue #28](https://github.com/marcel1702/hitomi-downloader/issues/28) is the roadmap: what is
done, what is planned and in which order. Every item is its own issue, branch and pull request.
An issue has one label of each of these two kinds:

| Where does it apply? | |
|---|---|
| `platform: web` | only the web version (server, Docker, browser) |
| `platform: desktop` | only the desktop app |
| `platform: all` | both, because the core or the frontend is shared |

| What is it? | |
|---|---|
| `bug` | something does not work |
| `enhancement` | a new function or an improvement |
| `extension` | together with `enhancement`: optional, not part of the core scope |
| `infra` | build, CI, release, Docker image |
| `documentation` | documentation only |

`roadmap` marks an overview issue such as #28 and needs no other label.

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
