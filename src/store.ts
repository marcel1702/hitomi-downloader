import { defineStore } from 'pinia'
import { Comic, commands, Config, SearchResult } from './bindings.ts'
import { computed, ref } from 'vue'
import { CurrentTabName, ProgressData } from './types.ts'
import { coverUrl, isWeb } from './platform.ts'
import { SubscriptionList, subscriptionCommands } from './subscriptions.ts'

export const useStore = defineStore('store', () => {
  const config = ref<Config>()
  const pickedComic = ref<Comic>()
  const currentTabName = ref<CurrentTabName>('search')
  const progresses = ref<Map<number, ProgressData>>(new Map())
  const covers = ref<Map<number, string>>(new Map())
  const searchResult = ref<SearchResult>()
  // the followed searches, web version only
  const subscriptions = ref<SubscriptionList>()
  const subscriptionErrors = computed(
    () => subscriptions.value?.subscriptions.filter((subscription) => subscription.status === 'error').length ?? 0,
  )

  async function loadSubscriptions() {
    try {
      subscriptions.value = await subscriptionCommands.list()
    } catch (err) {
      console.error(err)
    }
  }

  async function loadCover(id: number, url: string, comicDownloadDir?: string | null) {
    if (isWeb) {
      // the server finds the cover itself, and the browser caches it
      covers.value.set(id, coverUrl(id))
      return
    }
    const result = await commands.getCoverData(url, comicDownloadDir ?? null)
    if (result.status === 'error') {
      console.error(result.error)
      return
    }
    const coverData: number[] = result.data
    const coverBlob = new Blob([new Uint8Array(coverData)])
    const cover = URL.createObjectURL(coverBlob)
    covers.value.set(id, cover)
  }

  return {
    config,
    pickedComic,
    currentTabName,
    progresses,
    covers,
    loadCover,
    searchResult,
    subscriptions,
    subscriptionErrors,
    loadSubscriptions,
  }
})
