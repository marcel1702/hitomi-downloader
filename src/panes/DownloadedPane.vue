<script setup lang="tsx">
import { Comic, commands, events } from '../bindings.ts'
import { computed, onMounted, onUnmounted, ref, watch, nextTick } from 'vue'
import { MessageReactive, useMessage, DropdownOption, NIcon } from 'naive-ui'
import { open } from '@tauri-apps/plugin-dialog'
import { useStore } from '../store.ts'
import ComicCard from '../components/ComicCard.vue'
import { useI18n } from '../utils.ts'
import { PhChecks, PhFilePdf, PhFileZip, PhFolderOpen, PhTrash } from '@phosphor-icons/vue'
import { SelectionArea } from '@viselect/vue'
import { useMultiSelect } from '../composables/useMultiSelect.ts'
import { useRevealPath } from '../composables/useRevealPath.ts'
import { useDeleteComics } from '../composables/useDeleteComics.ts'
import { isWeb } from '../platform.ts'
import { filterComics, SORT_PRESETS, SortPreset, sortComics } from '../library.ts'

const { t, locale } = useI18n()

interface ProgressData {
  title: string
  progressMessage: MessageReactive
}

defineProps<{
  search: (query: string, pageNum: number) => Promise<void>
}>()

const store = useStore()

const message = useMessage()

const PAGE_SIZE = 20

// the whole library, in the order of the backend
const downloadedComics = ref<Comic[]>([])

// The search and the order of the list, see `library.ts`. The search follows the input a moment
// after the last key, the list is not sorted and filtered again for every letter.
const SEARCH_DELAY_MS = 200
const searchInput = ref<string>('')
const searchQuery = ref<string>('')
const sortPreset = ref<SortPreset>('downloaded_newest')
const sortOptions = computed(() => SORT_PRESETS.map((preset) => ({ label: t(`downloaded_pane.sort.${preset}`), value: preset })))
let searchTimer: ReturnType<typeof setTimeout> | undefined
watch(searchInput, () => {
  clearTimeout(searchTimer)
  // emptied, with the clear button above all, the whole library comes back at once
  if (searchInput.value.trim() === '') {
    searchQuery.value = ''
    return
  }
  searchTimer = setTimeout(() => {
    searchQuery.value = searchInput.value
  }, SEARCH_DELAY_MS)
})
onUnmounted(() => clearTimeout(searchTimer))

// the comics the search finds, in the chosen order. The pages, "Select All" and the export of
// all comics act on these.
const shownComics = computed<Comic[]>(() =>
  sortComics(filterComics(downloadedComics.value, searchQuery.value), sortPreset.value, locale.value),
)

const currentPage = ref<number>(1)
const pageCount = computed<number>(() => {
  return Math.ceil(shownComics.value.length / PAGE_SIZE)
})
// currentPageComics is a computed property that returns the comics on the current page
const currentPageComics = computed<Comic[]>(() => {
  const start = (currentPage.value - 1) * PAGE_SIZE
  const end = start + PAGE_SIZE
  return shownComics.value.slice(start, end)
})

// listen for changes in the tab and update the list of downloaded comics
watch(
  () => store.currentTabName,
  async () => {
    if (store.currentTabName !== 'downloaded') {
      return
    }

    downloadedComics.value = await commands.getDownloadedComics()
  },
  { immediate: true },
)

useProgressTracking()

function useProgressTracking() {
  const progresses = new Map<string, ProgressData>()

  async function handleExportPdfEvents() {
    await events.exportPdfEvent.listen(async ({ payload: exportEvent }) => {
      if (exportEvent.event === 'Start') {
        const { uuid, title } = exportEvent.data
        createProgress(uuid, title, t('downloaded_pane.pdf_exporting'))
      } else if (exportEvent.event === 'Error') {
        errorProgress(exportEvent.data.uuid, t('downloaded_pane.pdf_export_error'))
      } else if (exportEvent.event === 'End') {
        completeProgress(exportEvent.data.uuid, t('downloaded_pane.pdf_exported'))
      }
    })
  }

  async function handleExportCbzEvents() {
    await events.exportCbzEvent.listen(async ({ payload: exportEvent }) => {
      if (exportEvent.event === 'Start') {
        const { uuid, title } = exportEvent.data
        createProgress(uuid, title, t('downloaded_pane.cbz_exporting'))
      } else if (exportEvent.event === 'Error') {
        errorProgress(exportEvent.data.uuid, t('downloaded_pane.cbz_export_error'))
      } else if (exportEvent.event === 'End') {
        completeProgress(exportEvent.data.uuid, t('downloaded_pane.cbz_exported'))
      }
    })
  }

  // Create progress message
  function createProgress(uuid: string, title: string, actionMessage: string) {
    progresses.set(uuid, {
      title,
      progressMessage: message.loading(
        () => {
          const progressData = progresses.get(uuid)
          if (progressData === undefined) return ''
          return `${progressData.title} ${actionMessage}`
        },
        { duration: 0 },
      ),
    })
  }

  function errorProgress(uuid: string, actionMessage: string) {
    const progressData = progresses.get(uuid)
    if (progressData) {
      progressData.progressMessage.type = 'error'
      progressData.progressMessage.content = `${progressData.title} ${actionMessage}`
      setTimeout(() => {
        progressData.progressMessage.destroy()
        progresses.delete(uuid)
      }, 3000)
    }
  }

  // Set the progress message as complete
  function completeProgress(uuid: string, actionMessage: string) {
    const progressData = progresses.get(uuid)
    if (progressData) {
      progressData.progressMessage.type = 'success'
      progressData.progressMessage.content = `${progressData.title} ${actionMessage}`
      setTimeout(() => {
        progressData.progressMessage.destroy()
        progresses.delete(uuid)
      }, 3000)
    }
  }

  onMounted(async () => {
    await handleExportPdfEvents()
    await handleExportCbzEvents()
  })
}

const { revealPath } = useRevealPath()

async function selectExportDir() {
  // the export dir of the web version is a volume of the server
  if (store.config === undefined || isWeb) {
    return
  }

  const selectedDirPath = await open({ directory: true })
  if (selectedDirPath === null) {
    return
  }
  store.config.exportDir = selectedDirPath
}

async function showExportDirInFileManager() {
  if (store.config === undefined) {
    return
  }
  await revealPath(store.config.exportDir)
}

const { deleteComics } = useDeleteComics()

// the comics that are gone leave the list, and with them the selection
function removeFromList(comics: Comic[]) {
  const gone = new Set(comics.map((comic) => comic.id))
  downloadedComics.value = downloadedComics.value.filter((comic) => !gone.has(comic.id))
  // the last page may be empty now
  currentPage.value = Math.max(1, Math.min(currentPage.value, pageCount.value))
}

async function deleteSelection() {
  removeFromList(await deleteComics(selectedComics.value))
}

async function exportCbz(comics: Comic[]) {
  if (comics.length === 0) return
  message.info(t('downloaded_pane.export_all_cbz_started', { count: comics.length }))
  for (const comic of comics) {
    const result = await commands.exportCbz(comic)
    if (result.status === 'error') {
      console.error(result.error)
    }
  }
}

async function exportPdf(comics: Comic[]) {
  if (comics.length === 0) return
  message.info(t('downloaded_pane.export_all_pdf_started', { count: comics.length }))
  for (const comic of comics) {
    const result = await commands.exportPdf(comic)
    if (result.status === 'error') {
      console.error(result.error)
    }
  }
}

const {
  selectedIds,
  selectionAreaRef,
  selectableRefs,
  updateSelectedIds,
  unselectAll,
  onContextMenu,
  toggleSelection,
  scrollToTop,
  forgetElements,
  clearSelection,
} = useMultiSelect()

// a new page starts at its first comic, not where the previous page was scrolled to. The
// selection stays: it holds the comics of every page.
watch(currentPage, () => {
  forgetElements()
  scrollToTop()
})

// Another search or order starts on the first page. The selection stays here too, also for the
// comics the search hides.
watch([searchQuery, sortPreset], () => {
  if (currentPage.value !== 1) {
    currentPage.value = 1
    return
  }
  forgetElements()
  scrollToTop()
})

const { contextMenuX, contextMenuY, contextMenuShowing, contextMenuOptions, showContextMenu } = useContextMenu()

// a comic that is no longer in the list can not stay selected
watch(downloadedComics, () => {
  const ids = new Set(downloadedComics.value.map((comic) => comic.id))
  selectedIds.value = new Set([...selectedIds.value].filter((id) => ids.has(id)))
})

// The comics of all pages that are selected, in the order of the list
const selectedComics = computed<Comic[]>(() => downloadedComics.value.filter((comic) => selectedIds.value.has(comic.id)))
const hasSelection = computed<boolean>(() => selectedComics.value.length > 0)

// what the buttons above the list act on: the selection, or without one all comics the search finds
const targetComics = computed<Comic[]>(() => (hasSelection.value ? selectedComics.value : shownComics.value))

function selectAllPages() {
  selectedIds.value = new Set(shownComics.value.map((comic) => comic.id))
  // the cards of this page get their look from the selection area as well
  selectionAreaRef.value?.selection?.select(selectableRefs.value)
}


function useContextMenu() {
  const contextMenuX = ref<number>(0)
  const contextMenuY = ref<number>(0)
  const contextMenuShowing = ref<boolean>(false)
  const contextMenuOptions = computed<DropdownOption[]>(() => [
    {
      label: t('common.select_all'),
      key: 'select-all',
      icon: () => (
        <n-icon size="20">
          <PhChecks />
        </n-icon>
      ),
      props: {
        onClick: () => {
          if (selectionAreaRef.value === undefined) {
            return
          }
          selectAllPages()
          contextMenuShowing.value = false
        },
      },
    },
    {
      // in the web version the menu writes into the export folder of the server, the buttons
      // of a comic hand the file to the browser
      label: isWeb ? t('common.export_pdf_to_server') : t('common.export_pdf'),
      key: 'export-pdf',
      icon: () => (
        <n-icon size="20">
          <PhFilePdf />
        </n-icon>
      ),
      props: {
        onClick: () => {
          exportPdf(selectedComics.value)
          contextMenuShowing.value = false
        },
      },
    },
    {
      label: isWeb ? t('common.export_cbz_to_server') : t('common.export_cbz'),
      key: 'export-cbz',
      icon: () => (
        <n-icon size="20">
          <PhFileZip />
        </n-icon>
      ),
      props: {
        onClick: () => {
          exportCbz(selectedComics.value)
          contextMenuShowing.value = false
        },
      },
    },
  ])

  async function showContextMenu(e: MouseEvent) {
    contextMenuShowing.value = false
    await nextTick()
    contextMenuShowing.value = true
    contextMenuX.value = e.clientX
    contextMenuY.value = e.clientY
  }

  return {
    contextMenuX,
    contextMenuY,
    contextMenuShowing,
    contextMenuOptions,
    showContextMenu,
  }
}
</script>

<template>
  <div v-if="store.config !== undefined" class="h-full flex flex-col gap-2 box-border pt-2">
    <!-- the export folder of the web version is a volume of the server, the settings show it -->
    <n-input-group v-if="!isWeb" class="box-border px-2">
      <n-input-group-label size="small">
        {{ t('common.export_directory') }}
      </n-input-group-label>
      <n-input v-model:value="store.config.exportDir" size="small" readonly @click="selectExportDir" />
      <n-button class="w-9" size="small" :title="t('common.open_directory')" @click="showExportDirInFileManager">
        <template #icon>
          <n-icon size="20">
            <PhFolderOpen />
          </n-icon>
        </template>
      </n-button>
    </n-input-group>
    <!-- in the web version this is the first row, where the export folder of the desktop app is -->
    <div class="flex gap-2 px-2">
      <n-input
        v-model:value="searchInput"
        class="flex-1"
        size="small"
        clearable
        :placeholder="t('downloaded_pane.search_placeholder')" />
      <n-select class="w-44" size="small" v-model:value="sortPreset" :options="sortOptions" />
    </div>
    <div class="flex gap-2 px-2">
      <n-button size="small" :disabled="targetComics.length === 0" @click="exportCbz(targetComics)">
        <template #icon>
          <n-icon size="18"><PhFileZip /></n-icon>
        </template>
        {{ hasSelection ? t('downloaded_pane.export_selection_cbz') : t('downloaded_pane.export_all_cbz') }}
        ({{ targetComics.length }})
      </n-button>
      <n-button size="small" :disabled="targetComics.length === 0" @click="exportPdf(targetComics)">
        <template #icon>
          <n-icon size="18"><PhFilePdf /></n-icon>
        </template>
        {{ hasSelection ? t('downloaded_pane.export_selection_pdf') : t('downloaded_pane.export_all_pdf') }}
        ({{ targetComics.length }})
      </n-button>
      <n-button v-if="hasSelection" size="small" type="error" @click="deleteSelection">
        <template #icon>
          <n-icon size="18"><PhTrash /></n-icon>
        </template>
        {{ t('delete.selection') }} ({{ selectedComics.length }})
      </n-button>
      <n-button v-if="hasSelection" class="ml-auto" size="small" quaternary @click="clearSelection">
        {{ t('common.clear_selection') }}
      </n-button>
      <n-button v-else class="ml-auto" size="small" quaternary :disabled="shownComics.length === 0" @click="selectAllPages">
        <template #icon>
          <n-icon size="18"><PhChecks /></n-icon>
        </template>
        {{ t('common.select_all') }}
      </n-button>
    </div>

    <SelectionArea
      v-if="currentPageComics.length > 0"
      ref="selectionAreaRef"
      class="flex flex-col gap-row-2 overflow-auto box-border px-2 selection-container"
      :options="{ selectables: '.selectable', features: { deselectOnBlur: true } }"
      @contextmenu="showContextMenu"
      @move="updateSelectedIds"
      @start="unselectAll">
      <div
        v-for="comic in currentPageComics"
        :key="comic.id"
        ref="selectableRefs"
        :data-key="comic.id"
        :class="['selectable rounded p-[2px] relative', selectedIds.has(comic.id) ? 'selected' : '']"
        @contextmenu="() => onContextMenu(comic.id)">
        <comic-card :search="search" :comic="comic" show-export @deleted="removeFromList([$event])" />
        <div class="absolute top-2 left-2 z-10 bg-white/50 rounded flex items-center justify-center p-1" @mousedown.stop @touchstart.stop @pointerdown.stop @click.stop>
          <n-checkbox
            size="large"
            :checked="selectedIds.has(comic.id)"
            @update:checked="(checked: boolean) => toggleSelection(comic.id, checked)"
          />
        </div>
      </div>
      <n-dropdown
        placement="bottom-start"
        trigger="manual"
        :x="contextMenuX"
        :y="contextMenuY"
        :options="contextMenuOptions"
        :show="contextMenuShowing"
        :on-clickoutside="() => (contextMenuShowing = false)" />
    </SelectionArea>
    <n-empty
      v-else-if="searchQuery.trim() !== '' && downloadedComics.length > 0"
      class="flex-1 pt-8"
      :description="t('downloaded_pane.no_match')" />
    <div v-else class="flex-1"></div>

    <n-pagination
      class="box-border p-2 pt-0 mt-auto"
      :page-count="pageCount"
      :page="currentPage"
      @update:page="currentPage = $event" />
  </div>
</template>
