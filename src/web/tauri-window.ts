// Stands in for `@tauri-apps/api/window` in the web build, see `vite.config.ts`
//
// A browser tab has no Tauri window, `setAppFullscreen` in `platform.ts` takes the Fullscreen
// API of the browser instead and never calls this.
export function getCurrentWindow(): never {
  throw new Error('A browser tab has no Tauri window')
}
