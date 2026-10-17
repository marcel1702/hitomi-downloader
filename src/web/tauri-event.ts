// Stands in for `@tauri-apps/api/event` in the web build, see `vite.config.ts`
//
// The events arrive as Server-Sent Events from `GET /api/events`, one stream shared by every
// listener. The browser reconnects by itself after the connection broke, and the server then
// sends the running downloads first, so the download list catches up.

export interface Event<T> {
  event: string
  id: number
  payload: T
}

export type EventCallback<T> = (event: Event<T>) => void

export type UnlistenFn = () => void

let source: EventSource | undefined
let nextId = 1

function eventSource(): EventSource {
  source ??= new EventSource('/api/events', { withCredentials: true })
  return source
}

export async function listen<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
  const listener = (message: MessageEvent<string>) => {
    handler({ event, id: nextId++, payload: JSON.parse(message.data) as T })
  }
  eventSource().addEventListener(event, listener)
  return () => eventSource().removeEventListener(event, listener)
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
