// What differs between the desktop app and the web version
//
// `pnpm build:web` builds the frontend with the mode `web`, see `vite.config.ts`.

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
