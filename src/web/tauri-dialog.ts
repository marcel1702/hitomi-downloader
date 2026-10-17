// Stands in for `@tauri-apps/plugin-dialog` in the web build, see `vite.config.ts`
//
// The web version has no folder picker, download and export dir come from the volumes.
export async function open(): Promise<null> {
  return null
}
