<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { Comic, commands } from '../bindings.ts'
import { useStore } from '../store.ts'
import { useI18n } from '../utils.ts'
import DownloadButton from './DownloadButton.vue'
import { useRevealPath } from '../composables/useRevealPath.ts'
import { downloadUrl, isWeb, startBrowserDownload } from '../platform.ts'
import { useDeleteComics } from '../composables/useDeleteComics.ts'

const { t } = useI18n()

const props = withDefaults(
  defineProps<{
    comic: Comic
    search: (query: string, pageNum: number) => Promise<void>
    showExport?: boolean
  }>(),
  {
    showExport: false,
  },
)

const emit = defineEmits<{
  deleted: [comic: Comic]
}>()

const store = useStore()
const { deleteComics } = useDeleteComics()

const cover = computed<string | undefined>(() => store.covers.get(props.comic.id))

const isDownloaded = computed(() => props.comic.isDownloaded === true)

onMounted(() => {
  if (cover.value === undefined) {
    store.loadCover(props.comic.id, props.comic.coverUrl, props.comic.comicDownloadDir)
  }
})

async function pickComic() {
  store.pickedComic = props.comic
  store.currentTabName = 'comic'
}

const { revealPath, revealLabel } = useRevealPath()

async function showComicDownloadDirInFileManager() {
  const comicDownloadDir = props.comic.comicDownloadDir
  if (comicDownloadDir == null) return
  await revealPath(comicDownloadDir)
}

// The desktop app exports into the export folder, the web version hands the file to the
// browser, which saves it where the user sits. Writing into the folder on the server is the
// context menu and "Export All" of the local tab.
async function exportPdf() {
  if (isWeb) {
    startBrowserDownload(downloadUrl(props.comic.id, 'pdf'))
    return
  }
  const result = await commands.exportPdf(props.comic)
  if (result.status === 'error') console.error(result.error)
}

async function exportCbz() {
  if (isWeb) {
    startBrowserDownload(downloadUrl(props.comic.id, 'cbz'))
    return
  }
  const result = await commands.exportCbz(props.comic)
  if (result.status === 'error') console.error(result.error)
}

async function deleteComic() {
  const [deleted] = await deleteComics([props.comic])
  if (deleted !== undefined) emit('deleted', deleted)
}

function tagColor(female: number, male: number): string | undefined {
  if (female !== 0) return '#F472B6'
  if (male !== 0) return '#60A5FA'
  return undefined
}

function tagNs(female: number, male: number): string {
  if (female !== 0) return 'female'
  if (male !== 0) return 'male'
  return 'tag'
}
</script>

<template>
  <n-card content-style="padding: 0.25rem;" hoverable>
    <div class="flex h-full">
      <div class="w-24 flex-shrink-0 mr-4 cursor-pointer" @click="pickComic">
        <img
          v-if="cover !== undefined"
          class="w-full h-full object-contain transition-transform duration-200 hover:scale-108"
          :src="cover"
          alt="" />
        <n-skeleton v-else class="w-full h-full" :sharp="false" />
      </div>
      <div class="flex flex-col w-full overflow-hidden gap-row-1">
        <div
          class="font-bold text-xl line-clamp-2 cursor-pointer transition-colors duration-200 hover:text-blue-5"
          :title="comic.title"
          @click="pickComic">
          {{ comic.title }}
        </div>
        <div class="flex items-center gap-col-1">
          <div class="whitespace-nowrap">{{ t('common.artist') }}</div>
          <n-button
            v-for="(artist, index) in comic.artists"
            :key="index"
            size="tiny"
            @click="search(`artist:${artist.replace(' ', '_')}`, 1)">
            {{ artist }}
          </n-button>
        </div>
        <div class="flex items-center gap-col-1">
          <div class="whitespace-nowrap">{{ t('common.series') }}</div>
          <n-button
            v-for="(series, index) in comic.parodys"
            :key="index"
            size="tiny"
            @click="search(`series:${series.replace(' ', '_')}`, 1)">
            {{ series }}
          </n-button>
        </div>
        <div class="flex items-center gap-col-1">
          <div class="whitespace-nowrap">{{ t('common.type') }}</div>
          <n-button size="tiny" @click="search(`type:${comic.type.replace(' ', '_')}`, 1)">
            {{ comic.type }}
          </n-button>
        </div>
        <div class="flex items-center gap-col-1">
          <div class="whitespace-nowrap">{{ t('common.language') }}</div>
          <n-button
            v-if="comic.language !== ''"
            size="tiny"
            @click="search(`language:${comic.language.replace(' ', '_')}`, 1)">
            {{ comic.languageLocalname }}
          </n-button>
        </div>
        <div class="flex items-center gap-col-1">
          <div class="whitespace-nowrap">{{ t('common.tag') }}</div>
          <n-button
            v-for="({ tag, female, male }, index) in comic.tags"
            :key="index"
            size="tiny"
            :color="tagColor(female, male)"
            @click="search(`${tagNs(female, male)}:${tag.replace(' ', '_')}`, 1)">
            {{ tag }}
          </n-button>
        </div>
        <div class="flex items-center gap-col-1">
          <div>{{ comic.date }}</div>
          <div class="ml-auto">{{ comic.files.length }}P</div>
        </div>
        <div class="flex mt-auto gap-1">
          <n-button v-if="isDownloaded" size="tiny" @click="showComicDownloadDirInFileManager">
            {{ revealLabel }}
          </n-button>
          <template v-if="showExport && isDownloaded">
            <n-button type="primary" class="ml-auto" size="tiny" @click="exportPdf">
              {{ isWeb ? t('common.download_pdf') : t('common.export_pdf') }}
            </n-button>
            <n-button type="primary" size="tiny" @click="exportCbz">
              {{ isWeb ? t('common.download_cbz') : t('common.export_cbz') }}
            </n-button>
            <n-button type="error" size="tiny" @click="deleteComic">
              {{ t('delete.button') }}
            </n-button>
          </template>
          <download-button
            v-else
            type="primary"
            class="ml-auto"
            size="tiny"
            :comic-id="comic.id"
            :comic-downloaded="isDownloaded" />
        </div>
      </div>
    </div>
  </n-card>
</template>
