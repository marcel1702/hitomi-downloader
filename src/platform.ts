// What differs between the desktop app and the web version
//
// `pnpm build:web` builds the frontend with the mode `web`, see `vite.config.ts`.

import { getCurrentWindow } from '@tauri-apps/api/window'

/** Whether the frontend runs in a browser against the server instead of in the desktop app */
export const isWeb = import.meta.env.MODE === 'web'

/** The cover of a comic, served by the server, which knows whether it is downloaded */
export function coverUrl(comicId: number): string {
  return `/api/comics/${comicId}/cover`
}

/** Page `index` (0 based) of a comic, served by the server */
export function pageUrl(comicId: number, index: number): string {
  return `/api/comics/${comicId}/pages/${index}`
}

/**
 * Put the app into fullscreen or take it out of it
 *
 * The browser tab goes fullscreen through the Fullscreen API, which only allows it during a key
 * press or a click, so this is called before anything is awaited. The desktop app puts its Tauri
 * window into fullscreen, the Fullscreen API of the webview does not work on every platform.
 */
export async function setAppFullscreen(on: boolean): Promise<void> {
  if (!isWeb) {
    await getCurrentWindow().setFullscreen(on)
    return
  }
  if (on && document.fullscreenElement === null) {
    await document.documentElement.requestFullscreen()
  } else if (!on && document.fullscreenElement !== null) {
    await document.exitFullscreen()
  }
}

/** A comic as a file of `format`, built or served by the server for the browser to save */
export function downloadUrl(comicId: number, format: 'cbz' | 'pdf'): string {
  return `/api/comics/${comicId}/download/${format}`
}

/** How long a download may take to start, a PDF is built before the first byte is sent */
const DOWNLOAD_FRAME_LIFETIME_MS = 10 * 60 * 1000

/**
 * Let the browser save the file at `url`
 *
 * The file is loaded into a hidden frame, which the browser turns into a download because the
 * server marks it as an attachment. A link would do the same, but a failure of the server,
 * which answers with an error in place of the file, would then replace the whole page. Here it
 * only lands in the invisible frame, and the user sees it as the notification the server's
 * log event raises, as with every other error.
 */
export function startBrowserDownload(url: string) {
  const frame = document.createElement('iframe')
  frame.hidden = true
  frame.src = url
  document.body.appendChild(frame)
  setTimeout(() => frame.remove(), DOWNLOAD_FRAME_LIFETIME_MS)
}

/**
 * Put `text` on the clipboard, `false` if the browser does not allow it
 *
 * The clipboard API only exists on https and localhost. A server in the home network is
 * reached over plain http most of the time, there the older `execCommand` still works.
 */
export async function copyText(text: string): Promise<boolean> {
  if (navigator.clipboard !== undefined && window.isSecureContext) {
    try {
      await navigator.clipboard.writeText(text)
      return true
    } catch {
      // fall through to the older way
    }
  }

  const textarea = document.createElement('textarea')
  textarea.value = text
  textarea.setAttribute('readonly', '')
  textarea.style.position = 'fixed'
  textarea.style.opacity = '0'
  document.body.appendChild(textarea)
  textarea.select()
  try {
    return document.execCommand('copy')
  } catch {
    return false
  } finally {
    textarea.remove()
  }
}
