// The followed searches of the web version, see `crates/server/src/follow.rs`
//
// They are commands of the server only, the desktop app has neither these nor a tab for them,
// so they are not in `bindings.ts`. `invoke` is the one of the web build there: a failure is
// thrown as the `CommandError` the server answered with.
import { invoke } from '@tauri-apps/api/core'

export type SubscriptionStatus = 'active' | 'paused' | 'error' | 'checking'
export type FirstRun = 'fromNow' | 'everything'

export interface Subscription {
  id: string
  query: string
  status: SubscriptionStatus
  createdAt: number
  lastCheckAt: number | null
  /** Galleries the last check that worked put into the queue */
  lastNew: number
  matches: number
  lastError: string | null
  nextCheckAt: number | null
}

export interface SubscriptionList {
  /** None when the checks are off */
  intervalHours: number | null
  nextCheckAt: number | null
  subscriptions: Subscription[]
}

export interface SubscriptionPreview {
  query: string
  matches: number
  inLibrary: number
  toDownload: number
  alreadyFollowed: boolean
  canDownloadAll: boolean
  limit: number
}

export const subscriptionCommands = {
  list: () => invoke<SubscriptionList>('list_subscriptions'),
  preview: (query: string) => invoke<SubscriptionPreview>('preview_subscription', { query }),
  create: (query: string, firstRun: FirstRun) => invoke<Subscription>('create_subscription', { query, firstRun }),
  remove: (id: string) => invoke<null>('delete_subscription', { id }),
  setPaused: (id: string, paused: boolean) => invoke<null>('set_subscription_paused', { id, paused }),
  check: (id: string) => invoke<null>('check_subscription', { id }),
  checkAll: () => invoke<null>('check_all_subscriptions'),
}

/** What went wrong, from the error a command threw */
export function errorText(error: unknown): string {
  if (typeof error === 'object' && error !== null) {
    const { err_message: message } = error as { err_message?: unknown }
    if (typeof message === 'string' && message !== '') return message
    if (error instanceof Error) return error.message
  }
  return String(error)
}

/** "2 hours ago", "in 5 minutes", in the language of the app, for a time in seconds since 1970 */
export function relativeTime(seconds: number, locale: string, nowMs: number = Date.now()): string {
  const format = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' })
  const diff = seconds - nowMs / 1000
  const abs = Math.abs(diff)
  if (abs < 60) return format.format(Math.round(diff), 'second')
  if (abs < 3600) return format.format(Math.round(diff / 60), 'minute')
  if (abs < 86400) return format.format(Math.round(diff / 3600), 'hour')
  return format.format(Math.round(diff / 86400), 'day')
}
