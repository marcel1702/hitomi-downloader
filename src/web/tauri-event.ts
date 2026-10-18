// Stands in for `@tauri-apps/api/event` in the web build, see `vite.config.ts`
//
// The events arrive as Server-Sent Events from `GET /api/events`, one stream shared by every
// listener. The browser reconnects by itself after the connection broke, and the server then
// sends the running downloads first, so the download list catches up.

import { isLeaving, loggedIn, toLogin } from './session.ts'

export interface Event<T> {
  event: string
  id: number
  payload: T
}

export type EventCallback<T> = (event: Event<T>) => void

export type UnlistenFn = () => void

// Every listener, so they can move to a new stream after the old one gave up
const listeners = new Map<(message: MessageEvent<string>) => void, string>()
let source: EventSource | undefined
let nextId = 1

function eventSource(): EventSource {
  if (source === undefined) {
    source = new EventSource('/api/events', { withCredentials: true })
    for (const [listener, event] of listeners) {
      source.addEventListener(event, listener)
    }
    source.addEventListener('error', onError)
  }
  return source
}

/**
 * A stream that broke off reconnects by itself. One the server answered with an error, a
 * `401` after the session ended above all, is closed for good: the login page or a new stream
 * takes over then.
 */
async function onError() {
  if (isLeaving() || source?.readyState !== EventSource.CLOSED) {
    return
  }
  source = undefined
  try {
    if (!(await loggedIn())) {
      toLogin()
    }
  } catch {
    // the server is not reachable, the new stream keeps trying
  }
  setTimeout(eventSource, 3000)
}

export async function listen<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
  const listener = (message: MessageEvent<string>) => {
    handler({ event, id: nextId++, payload: JSON.parse(message.data) as T })
  }
  listeners.set(listener, event)
  eventSource().addEventListener(event, listener)
  return () => {
    listeners.delete(listener)
    source?.removeEventListener(event, listener)
  }
}

export async function once<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
  const unlisten = await listen<T>(event, (received) => {
    unlisten()
    handler(received)
  })
  return unlisten
}

// The frontend never sends events to the backend
export async function emit(): Promise<void> {}
