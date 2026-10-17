// Stands in for `@tauri-apps/api/path` and the `path` of `@tauri-apps/api` in the web build,
// see `vite.config.ts`
//
// The paths only serve the buttons that open the config and log folder in a file manager,
// which the web version does not show: the folders are on the server.

function notInTheWebVersion(): never {
  throw new Error('the web version has no access to the folders on the server')
}

export async function appDataDir(): Promise<string> {
  notInTheWebVersion()
}

export async function join(): Promise<string> {
  notInTheWebVersion()
}
