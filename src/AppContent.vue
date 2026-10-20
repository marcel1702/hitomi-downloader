<script setup lang="tsx">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import type { DropdownOption, TabsInst } from 'naive-ui'
import { NIcon, useMessage, useNotification } from 'naive-ui'
import { commands } from './bindings.ts'
import { isWeb } from './platform.ts'
import { logOut } from './web/session.ts'
import { useStore } from './store.ts'
import LogViewer from './components/LogViewer.vue'
import { useI18n } from './utils.ts'
import AboutDialog from './components/AboutDialog.vue'
import SearchPane from './panes/SearchPane.vue'
import DownloadedPane from './panes/DownloadedPane.vue'
import ComicPane from './panes/ComicPane.vue'
import ReaderPane from './panes/ReaderPane.vue'
import DownloadingPane from './panes/DownloadingPane.vue'
import SubscriptionsPane from './panes/SubscriptionsPane.vue'
import { PhClockCounterClockwise, PhDotsThreeOutline, PhInfo, PhSignOut, PhTranslate } from '@phosphor-icons/vue'
import { locales, resolveLocale } from './locales'
import { useDevice } from './composables/useDevice.ts'

const { t, locale } = useI18n()
const store = useStore()

const message = useMessage()
const notification = useNotification()

const logViewerShowing = ref<boolean>(false)
const aboutDialogShowing = ref<boolean>(false)

const localeOptions = Object.entries(locales).map(([key, value]) => ({
  label: value.name,
  value: key,
}))

// On a narrow screen the downloads are a tab of their own and the buttons above them a menu
const { narrow } = useDevice()
const runningDownloads = computed(
  () =>
    Array.from(store.progresses.values()).filter(({ state }) => state === 'Downloading' || state === 'Pending')
      .length,
)
// back to two columns, the downloads tab is gone
watch(narrow, (isNarrow) => {
  if (!isNarrow && store.currentTabName === 'downloads') store.currentTabName = 'search'
})
// The downloads pane moves into its tab once the tab is rendered. The element, not a selector:
// the teleport looks a selector up only once, and the tab is a new element after each switch.
const downloadsTab = ref<HTMLElement | null>(null)
const menuOptions = computed<DropdownOption[]>(() => [
  { key: 'log', label: t('log_viewer.name'), icon: () => <NIcon size="20"><PhClockCounterClockwise /></NIcon> },
  { key: 'about', label: t('about_dialog.name'), icon: () => <NIcon size="20"><PhInfo /></NIcon> },
  {
    key: 'language',
    label: t('app_content.language'),
    icon: () => <NIcon size="20"><PhTranslate /></NIcon>,
    children: localeOptions.map(({ label, value }) => ({ key: `language:${value}`, label })),
  },
  ...(isWeb ? [{ key: 'logout', label: t('app_content.log_out'), icon: () => <NIcon size="20"><PhSignOut /></NIcon> }] : []),
])
function onMenuSelect(key: string) {
  if (key === 'log') logViewerShowing.value = true
  else if (key === 'about') aboutDialogShowing.value = true
  else if (key === 'logout') logOut()
  else if (key.startsWith('language:')) locale.value = resolveLocale(key.slice('language:'.length))
}

const searchPaneRef = ref<InstanceType<typeof SearchPane>>()
const tabsInstRef = ref<TabsInst | null>(null)
/** Push the tray labels to the backend, which cannot read the locale files itself */
async function syncTrayMenuLabels() {
  // the web version has no tray
  if (isWeb) {
    return
  }
  const result = await commands.updateTrayMenuLabels(t('tray.show_window'), t('tray.quit'))
  if (result.status === 'error') {
    console.error(result.error)
  }
}

watch(
  locale,
  () => {
    tabsInstRef.value?.syncBarPosition()
    if (store.config !== undefined) {
      store.config.language = locale.value
    }
    syncTrayMenuLabels()
  },
  { flush: 'post' },
)

watch(
  () => store.config,
  async () => {
    if (store.config === undefined) {
      return
    }
    const result = await commands.saveConfig(store.config)
    if (result.status === 'error') {
      // The backend logs the error, which the log viewer already shows as a notification
      console.error(result.error)
      return
    }
    message.success(() => t('app_content.save_config_success'))
  },
  { deep: true },
)

// The badge of the followed searches counts those with an error. The tab loads the list when
// it is shown, this is for the time it is not.
let subscriptionTimer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  if (!isWeb) return
  store.loadSubscriptions()
  subscriptionTimer = setInterval(() => {
    if (store.currentTabName !== 'following') store.loadSubscriptions()
  }, 60_000)
})
onUnmounted(() => clearInterval(subscriptionTimer))

onMounted(async () => {
  // block the browser right-click menu
  document.oncontextmenu = (event) => {
    event.preventDefault()
  }
  // get the configuration
  store.config = await commands.getConfig()
  // restore the language the user picked last time, falling back to English for a
  // config that names a locale this version no longer ships
  locale.value = resolveLocale(store.config.language)
  // the watcher above only fires on a change, so the tray needs one explicit sync
  await syncTrayMenuLabels()
  // check the logs directory size
  const result = await commands.getLogsDirSize()
  if (result.status === 'error') {
    console.error(result.error)
    return
  }
  if (result.data > 50 * 1024 * 1024) {
    notification.warning({
      title: () => t('app_content.logs_directory_size_too_big'),
      description: () => t('app_content.log_cleanup_reminder'),
      content: () => (
        <>
          <div>
            {t('app_content.click_top_center')}
            <span class="bg-gray-2 px-1">{t('log_viewer.name')}</span>
          </div>
          <div>
            {t('app_content.there_is')}
            <span class="bg-gray-2 px-1">{t('log_viewer.open_logs_directory')}</span>
          </div>
          <div>
            {t('app_content.you_can_also_uncheck')}
            <span class="bg-gray-2 px-1">{t('log_viewer.enable_file_logging')}</span>
          </div>
          <div>{t('app_content.this_will_disable_file_logging')}</div>
        </>
      ),
    })
  }
})
</script>

<template>
  <div v-if="store.config !== undefined" class="app-root h-screen flex flex-col">
    <div class="flex flex-1 overflow-hidden">
      <n-tabs
        ref="tabsInstRef"
        class="h-full flex-[3] min-w-0"
        v-model:value="store.currentTabName"
        type="line"
        size="small"
        animated>
        <n-tab-pane
          class="h-full overflow-auto p-0!"
          name="search"
          :tab="t('search_pane.name')"
          display-directive="show">
          <searchPane ref="searchPaneRef" />
        </n-tab-pane>
        <n-tab-pane
          class="h-full overflow-auto p-0!"
          name="downloaded"
          :tab="t('downloaded_pane.name')"
          display-directive="show">
          <downloaded-pane v-if="searchPaneRef !== undefined" :search="searchPaneRef.search" />
        </n-tab-pane>
        <n-tab-pane class="h-full overflow-auto p-0!" name="comic" :tab="t('comic_pane.name')" display-directive="show">
          <comic-pane v-if="searchPaneRef !== undefined" :search="searchPaneRef.search" />
        </n-tab-pane>
        <n-tab-pane class="h-full overflow-hidden p-0!" name="reader" :tab="t('reader_pane.name')" display-directive="show">
          <reader-pane />
        </n-tab-pane>
        <!-- a check needs a server that keeps running, the desktop app has no such tab -->
        <n-tab-pane
          v-if="isWeb"
          class="h-full overflow-auto p-0!"
          name="following"
          display-directive="show">
          <template #tab>
            <n-badge :value="store.subscriptionErrors" :show="store.subscriptionErrors > 0" :offset="[10, -2]">
              {{ t('following_pane.name') }}
            </n-badge>
          </template>
          <subscriptions-pane v-if="searchPaneRef !== undefined" :search="searchPaneRef.search" />
        </n-tab-pane>
        <!-- one column: the downloads of the right column move in here, see the teleport below -->
        <n-tab-pane v-if="narrow" class="h-full overflow-hidden p-0!" name="downloads" display-directive="show">
          <template #tab>
            <n-badge :value="runningDownloads" :show="runningDownloads > 0" :offset="[10, -2]">
              {{ t('downloading_pane.name') }}
            </n-badge>
          </template>
          <div ref="downloadsTab" id="downloads-tab" class="h-full flex flex-col" />
        </n-tab-pane>
        <template v-if="narrow" #suffix>
          <n-dropdown trigger="click" placement="bottom-end" :options="menuOptions" @select="onMenuSelect">
            <n-button quaternary circle :title="t('app_content.menu')">
              <template #icon>
                <n-icon size="20">
                  <PhDotsThreeOutline />
                </n-icon>
              </template>
            </n-button>
          </n-dropdown>
        </template>
      </n-tabs>
      <div v-show="!narrow" class="flex-[2] min-w-0 overflow-auto h-full flex flex-col border-l border-gray-2">
        <div class="flex gap-2 px-2 py-1 box-border items-center border-b border-gray-2 bg-gray-50">
          <n-button size="small" quaternary @click="logViewerShowing = true">
            <template #icon>
              <n-icon size="20">
                <PhClockCounterClockwise />
              </n-icon>
            </template>
            {{ t('log_viewer.name') }}
          </n-button>
          <n-button size="small" quaternary @click="aboutDialogShowing = true">
            <template #icon>
              <n-icon size="20">
                <PhInfo />
              </n-icon>
            </template>
            {{ t('about_dialog.name') }}
          </n-button>
          <!-- the web version sits behind a login, the desktop app does not -->
          <n-button v-if="isWeb" size="small" quaternary @click="logOut">
            <template #icon>
              <n-icon size="20">
                <PhSignOut />
              </n-icon>
            </template>
            {{ t('app_content.log_out') }}
          </n-button>
          <div class="ml-auto flex items-center gap-1">
            <n-icon size="22">
              <PhTranslate />
            </n-icon>
            <n-select class="w-28" size="small" v-model:value="locale" :options="localeOptions" />
          </div>
        </div>
        <!-- moved, not mounted again: the pane listens to the download events for the whole app -->
        <Teleport :to="downloadsTab ?? 'body'" :disabled="downloadsTab === null">
          <downloading-pane
            class="flex-1 overflow-auto"
            v-if="searchPaneRef !== undefined"
            :search="searchPaneRef.search" />
        </Teleport>
      </div>
    </div>
    <log-viewer v-model:showing="logViewerShowing" />
    <about-dialog v-model:showing="aboutDialogShowing" />
  </div>
</template>
<style scoped>
:global(.n-notification-main__header) {
  @apply break-words;
}

:global(.n-tabs-pane-wrapper) {
  @apply h-full;
}

:deep(.n-tabs-nav) {
  @apply px-2;
}

/* one column on a phone: the visible height without the bars of the browser, clear of a notch */
:global([data-narrow]) .app-root {
  height: 100dvh;
  padding: env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom)
    env(safe-area-inset-left);
  box-sizing: border-box;
}
</style>
