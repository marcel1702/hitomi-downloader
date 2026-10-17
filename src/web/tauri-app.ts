// Stands in for `@tauri-apps/api/app` in the web build, see `vite.config.ts`

/** The version of the server, which is the version of the app */
export async function getVersion(): Promise<string> {
  const response = await fetch('/api/health', { credentials: 'same-origin' })
  const health = (await response.json()) as { version: string }
  return health.version
}
