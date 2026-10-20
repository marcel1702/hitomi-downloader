<script setup lang="ts">
import { useStore } from '../store.ts'
import { Comic, commands } from '../bindings.ts'
import { computed, watch, ref, nextTick } from 'vue'
import { useI18n } from '../utils.ts'
import { useMessage } from 'naive-ui'
import DownloadButton from '../components/DownloadButton.vue'
import ComicCard from '../components/ComicCard.vue'
import { useRevealPath } from '../composables/useRevealPath.ts'
import { isWeb } from '../platform.ts'
import { useDeleteComics } from '../composables/useDeleteComics.ts'

const { t } = useI18n()

const message = useMessage()

defineProps<{
  search: (query: string, pageNum: number) => Promise<void>
}>()

const store = useStore()
const { deleteComics } = useDeleteComics()

const relatedComics = ref<Comic[]>([])
const containerRef = ref<HTMLElement>()

const cover = computed<string | undefined>(() =>
  store.pickedComic ? store.covers.get(store.pickedComic.id) : undefined,
)

watch(
  () => store.pickedComic,
  () => {
    if (store.pickedComic === undefined) {
      return
    }
    if (containerRef.value !== undefined) {
      nextTick(() => containerRef.value?.scrollTo({ top: 0, behavior: 'instant' }))
    }

    if (cover.value === undefined) {
      store.loadCover(store.pickedComic.id, store.pickedComic.coverUrl, store.pickedComic.comicDownloadDir)
    }

    reloadRelatedComics()
  },
)

async function pickComic(id: number) {
  const result = await commands.getComic(id)
  if (result.status === 'error') {
    console.error(result.error)
    return
  }
  store.pickedComic = result.data
}

async function reloadRelatedComics() {
  if (store.pickedComic === undefined) {
    return
  }

  relatedComics.value = []

  const promises = store.pickedComic.related.map(async (id) => {
    const result = await commands.getComic(id)
    if (result.status === 'error') {
      console.error(result.error)
      return
    }
    relatedComics.value.push(result.data)
  })

  await Promise.all(promises)
}

const { revealPath } = useRevealPath()

// the path of the comic on the server, shown in a dialog of the web version
const shownPath = ref<string>()
const pathDialogShowing = ref(false)

/** Open the folder of the comic in the file manager, the web version shows its path */
async function showComicDownloadDir() {
  if (store.pickedComic === undefined || store.config === undefined) {
    return
  }

  const comicDownloadDir = store.pickedComic.comicDownloadDir
  if (comicDownloadDir === undefined || comicDownloadDir === null) {
    // No backend call failed here, so nothing else would tell the user why the click did nothing
    message.warning(() => t('comic_pane.no_download_dir'))
    return
  }

  if (isWeb) {
    shownPath.value = comicDownloadDir
    pathDialogShowing.value = true
    return
  }
  await revealPath(comicDownloadDir)
}

// the whole path is selected at once, ready to be copied
function selectAll(event: FocusEvent) {
  const input = event.target as HTMLTextAreaElement
  input.select()
}
</script>

<template>
  <n-empty
    class="p-2"
    v-if="store.pickedComic === undefined"
    :description="t('comic_pane.empty_description')"></n-empty>
  <div ref="containerRef" v-else class="flex flex-col h-full overflow-auto box-border p-2">
    <span class="font-bold text-xl">{{ store.pickedComic.title }}</span>
    <div class="flex w-full">
      <img v-if="cover !== undefined" class="w-40 object-cover mr-2" :src="cover" alt="" />
      <n-skeleton v-else class="w-40 h-56 mr-2" :sharp="false" />
      <div class="flex flex-col w-full gap-1">
        <div>
          <!-- TODO: format the date with i18n -->
          {{ store.pickedComic.date }}
        </div>
        <div class="flex">
          <div>ID: {{ store.pickedComic.id }}</div>
          <div class="ml-auto">{{ store.pickedComic.files.length }}P</div>
        </div>
        <div class="flex flex-col mt-auto mb-2">
          <div class="font-bold">{{ t('comic_pane.other_language') }}</div>
          <div class="flex flex-wrap gap-1">
            <n-button
              v-for="(language, index) in store.pickedComic.languages"
              :key="index"
              size="tiny"
              round
              class="hover:scale-110 transition-transform duration-100"
              @click="pickComic(language.galleryid)">
              {{ language.language_localname }}
            </n-button>
          </div>
        </div>
        <div class="flex flex-col gap-row-2">
          <n-button size="small" type="info" @click="store.currentTabName = 'reader'">
            {{ t('comic_pane.read') }}
          </n-button>
          <download-button
            size="small"
            type="primary"
            :comic-id="store.pickedComic.id"
            :comic-downloaded="store.pickedComic.isDownloaded === true" />
          <n-button v-if="store.pickedComic.isDownloaded" size="small" type="error" @click="deleteComics([store.pickedComic])">
            {{ t('delete.button') }}
          </n-button>
          <!-- one name for one question, where the comic lies, answered as the platform can -->
          <n-button
            v-if="store.pickedComic.isDownloaded"
            size="small"
            :title="isWeb ? t('common.location_show') : t('common.location_open')"
            @click="showComicDownloadDir">
            {{ t('common.location') }}
          </n-button>
        </div>
      </div>
    </div>
    <div class="flex flex-col gap-2 mt-1 pb-2">
      <div>
        <div class="font-bold">{{ t('common.artist') }}</div>
        <div class="flex flex-wrap gap-1">
          <n-button
            v-for="(artist, index) in store.pickedComic.artists"
            :key="index"
            size="tiny"
            round
            class="hover:scale-110 transition-transform duration-100"
            @click="search(`artist:${artist.replace(' ', '_')}`, 1)">
            {{ artist }}
          </n-button>
        </div>
      </div>
      <div>
        <div class="font-bold">{{ t('common.group') }}</div>
        <div class="flex flex-wrap gap-1">
          <n-button
            v-for="(group, index) in store.pickedComic.groups"
            :key="index"
            size="tiny"
            round
            class="hover:scale-110 transition-transform duration-100"
            @click="search(`group:${group.replace(' ', '_')}`, 1)">
            {{ group }}
          </n-button>
        </div>
      </div>
      <div>
        <div class="font-bold">{{ t('common.type') }}</div>
        <n-button
          size="tiny"
          round
          class="hover:scale-110 transition-transform duration-100"
          @click="search(`type:${store.pickedComic.type.replace(' ', '_')}`, 1)">
          {{ store.pickedComic.type }}
        </n-button>
      </div>
      <div>
        <div class="font-bold">{{ t('common.language') }}</div>
        <n-button
          v-if="store.pickedComic.language !== ''"
          size="tiny"
          round
          class="hover:scale-110 transition-transform duration-100"
          @click="search(`language:${store.pickedComic.language.replace(' ', '_')}`, 1)">
          {{ store.pickedComic.languageLocalname }}
        </n-button>
      </div>
      <div>
        <div class="font-bold">{{ t('common.series') }}</div>
        <div class="flex flex-wrap gap-1">
          <n-button
            v-for="(series, index) in store.pickedComic.parodys"
            :key="index"
            round
            class="hover:scale-110 transition-transform duration-100"
            size="tiny"
            @click="search(`series:${series.replace(' ', '_')}`, 1)">
            {{ series }}
          </n-button>
        </div>
      </div>
      <div>
        <div class="font-bold">{{ t('common.character') }}</div>
        <div class="flex flex-wrap gap-1">
          <n-button
            v-for="(character, index) in store.pickedComic.characters"
            :key="index"
            round
            class="hover:scale-110 transition-transform duration-100"
            size="tiny"
            @click="search(`character:${character.replace(' ', '_')}`, 1)">
            {{ character }}
          </n-button>
        </div>
      </div>
      <div>
        <div class="font-bold">{{ t('common.tag') }}</div>
        <div class="flex flex-wrap gap-1">
          <n-button
            v-for="({ tag, female, male }, index) in store.pickedComic.tags"
            :key="index"
            round
            class="hover:scale-110 transition-transform duration-100'"
            :color="female !== 0 ? '#F472B6' : male !== 0 ? '#60A5FA' : undefined"
            size="tiny"
            @click="search(`${female !== 0 ? 'female' : male !== 0 ? 'male' : 'tag'}:${tag.replace(' ', '_')}`, 1)">
            {{ tag }}
          </n-button>
        </div>
      </div>
      <div>
        <div class="font-bold">{{ t('comic_pane.related') }}</div>
        <div class="flex flex-wrap gap-1">
          <ComicCard v-for="comic in relatedComics" :key="comic.id" :comic="comic" :search="search" />
        </div>
      </div>
    </div>
  </div>
  <n-modal v-model:show="pathDialogShowing">
    <n-dialog class="app-dialog" :showIcon="false" :title="t('comic_pane.path_dialog_title')" @close="pathDialogShowing = false">
      <!-- wrapped, so that a long path is seen whole -->
      <n-input
        class="path font-mono"
        type="textarea"
        :value="shownPath"
        :autosize="{ minRows: 1 }"
        size="small"
        readonly
        autofocus
        @focus="selectAll" />
    </n-dialog>
  </n-modal>
</template>

<style scoped>
/* a path has few spaces, it breaks anywhere */
.path :deep(textarea) {
  word-break: break-all;
}
</style>
