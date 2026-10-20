// The notifications of the web version through Apprise, see `crates/server/src/notify.rs`
//
// Commands of the server only, like Kavita, so they are not in `bindings.ts`. The Apprise URLs
// hold the tokens of the services: they are sent, never received, the server only counts them.
import { invoke } from '@tauri-apps/api/core'

export interface NotifySettings {
  url: string
  urlCount: number
  onCompleted: boolean
  onFailed: boolean
  onFollowed: boolean
  censored: boolean
}

/** What the dialog sends, `urls` of `null` keeps the Apprise URLs the server has */
export interface NotifySettingsArgs {
  url: string
  urls: string[] | null
  onCompleted: boolean
  onFailed: boolean
  onFollowed: boolean
  censored: boolean
}

export const notifyCommands = {
  get: () => invoke<NotifySettings>('get_notify_settings'),
  save: (args: NotifySettingsArgs) => invoke<NotifySettings>('save_notify_settings', { ...args }),
  test: (args: NotifySettingsArgs) => invoke<null>('test_notify', { ...args }),
}
