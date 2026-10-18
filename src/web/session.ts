// The login of the web version, see `crates/server/src/auth`

let leaving = false

// A browser that leaves the page breaks its open connections off, which looks like a failure
// to whoever listens to them. Nothing is to be done about those, the page is gone anyway.
addEventListener('pagehide', () => {
  leaving = true
})

/** Whether the page is on its way to another one */
export function isLeaving(): boolean {
  return leaving
}

/** Send the browser to the login page, which brings it back here afterwards */
export function toLogin(): never {
  leaving = true
  const here = location.pathname + location.search
  location.assign(`/auth/login?next=${encodeURIComponent(here)}`)
  // nothing that waited for an answer may go on as if it got one
  throw new Error('not logged in')
}

/** Whether the browser still has a session */
export async function loggedIn(): Promise<boolean> {
  const response = await fetch('/auth/session', { credentials: 'same-origin' })
  return response.ok
}

/** End the session and show the login page */
export async function logOut(): Promise<void> {
  leaving = true
  await fetch('/auth/logout', { method: 'POST', credentials: 'same-origin' })
  location.assign('/auth/login')
}
