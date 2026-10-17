// Stands in for `@tauri-apps/api/core` in the web build, see `vite.config.ts`
//
// A command is a `POST /api/<command>` to the server, with the arguments as JSON. The server
// answers like Tauri does: the value on success, the `CommandError` on failure, which is
// thrown as a plain object, so `bindings.ts` turns it into `{ status: 'error' }` as usual.
// Without a session it sends the browser to the login page.

import { toLogin } from './session.ts'

export async function invoke<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  const response = await fetch(`/api/${cmd}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(args),
    credentials: 'same-origin',
  })
  if (response.status === 401) {
    toLogin()
  }
  const text = await response.text()

  let body: unknown = null
  if (text !== '') {
    try {
      body = JSON.parse(text)
    } catch {
      // not an answer of the server, a reverse proxy error page for example
      throw new Error(`${cmd} failed: HTTP ${response.status}`)
    }
  }

  if (!response.ok) {
    throw body ?? new Error(`${cmd} failed: HTTP ${response.status}`)
  }
  return body as T
}

// Only named in the type signatures of `bindings.ts`, no command uses a channel
export class Channel<T> {
  onmessage: (response: T) => void = () => {}
}
