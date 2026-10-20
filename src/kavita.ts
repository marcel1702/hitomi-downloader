// The Kavita of the web version, see `crates/server/src/kavita.rs`
//
// Commands of the server only, like the followed searches, so they are not in `bindings.ts`.
// The API key is sent, never received: the server only says whether there is one.
import { invoke } from '@tauri-apps/api/core'

export interface KavitaSettings {
  url: string
  libraryId: number | null
  hasApiKey: boolean
}

export interface KavitaLibrary {
  id: number
  name: string
}

/** What the dialog sends, an `apiKey` of `null` keeps the one the server has */
export interface KavitaSettingsArgs {
  url: string
  apiKey: string | null
  libraryId: number | null
}

export const kavitaCommands = {
  get: () => invoke<KavitaSettings>('get_kavita_settings'),
  save: (args: KavitaSettingsArgs) => invoke<KavitaSettings>('save_kavita_settings', { ...args }),
  test: (args: KavitaSettingsArgs) => invoke<KavitaLibrary[]>('test_kavita', { ...args }),
}
