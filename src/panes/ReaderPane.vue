<script setup lang="ts">
import { useStore } from '../store.ts'
import { Comic, commands } from '../bindings.ts'
import { isWeb, pageUrl } from '../platform.ts'
import { ref, watch, onMounted, onUnmounted, computed, nextTick } from 'vue'
import {
  PhCaretLeft,
  PhCaretRight,
  PhList,
  PhRows,
  PhArrowsOutSimple,
  PhImages,
  PhPlay,
  PhPause,
} from '@phosphor-icons/vue'
import { useI18n } from '../utils.ts'

const { t } = useI18n()
const store = useStore()

type ReadMode = 'page' | 'scroll'

const currentIndex = ref(0)
const currentImage = ref<string | undefined>(undefined)
const loading = ref(false)
const readMode = ref<ReadMode>('page')
const thumbnailsShowing = ref(false)
const fitWidth = ref(false)
const scrollContainerRef = ref<HTMLElement>()
const scrollImages = ref<Map<number, string>>(new Map())

const imageCache = new Map<number, Promise<string | undefined>>()

const comic = computed(() => store.pickedComic)
const totalFiles = computed(() => comic.value?.files.length ?? 0)

// The reader is dark whatever the theme of the app is. The buttons are solid, with white
// icons, so that they stand out from the bar they sit on.
const BUTTON_COLOR = '#6B7280'
const BUTTON_COLOR_ACTIVE = '#6275C0'
const buttonColor = (active: boolean) => (active ? BUTTON_COLOR_ACTIVE : BUTTON_COLOR)

function clearCache() {
  for (const promise of imageCache.values()) {
    promise.then(url => { if (url) URL.revokeObjectURL(url) }).catch(() => {})
  }
  imageCache.clear()
  scrollImages.value.clear()
  resetThumbnails()
}

/** The image of page `index` of `c`, from the server, from the archive or from Hitomi */
async function fetchPageBlob(c: Comic, index: number): Promise<Blob | undefined> {
  if (isWeb) {
    // the server finds the page itself, from the archive or from Hitomi
    const response = await fetch(pageUrl(c.id, index), { credentials: 'same-origin' })
    // the server reports the error as a log event, which shows it
    return response.ok ? await response.blob() : undefined
  }
  const result = await commands.getImageData(
    c.id,
    index,
    c.isDownloaded ?? null,
    c.comicDownloadDir ?? null,
    c.files[index],
  )
  if (result.status === 'ok') {
    return new Blob([new Uint8Array(result.data)])
  }
  console.error(result.error)
  return undefined
}

function getOrFetchImage(index: number): Promise<string | undefined> {
  if (!comic.value || index < 0 || index >= comic.value.files.length) {
    return Promise.resolve(undefined)
  }
  if (imageCache.has(index)) {
    return imageCache.get(index)!
  }

  const promise = (async () => {
    const c = comic.value
    if (!c) return undefined
    const blob = await fetchPageBlob(c, index)
    if (blob === undefined) {
      imageCache.delete(index)
      return undefined
    }
    return URL.createObjectURL(blob)
  })()

  imageCache.set(index, promise)
  return promise
}

async function loadCurrentImage() {
  if (!comic.value || comic.value.files.length === 0) return
  loading.value = true
  const index = currentIndex.value

  const url = await getOrFetchImage(index)
  if (currentIndex.value !== index || store.pickedComic !== comic.value) return

  if (store.currentTabName === 'reader') {
    currentImage.value = url
  }
  loading.value = false

  // Preload adjacent images
  for (const i of [index + 1, index + 2, index - 1]) {
    if (i >= 0 && i < comic.value.files.length) {
      getOrFetchImage(i).catch(() => {})
    }
  }

  // Cleanup distant cache entries
  for (const key of imageCache.keys()) {
    if (key < index - 5 || key > index + 5) {
      const promise = imageCache.get(key)
      if (promise) {
        promise.then(u => { if (u) URL.revokeObjectURL(u) }).catch(() => {})
      }
      imageCache.delete(key)
    }
  }
}

async function loadScrollImages() {
  if (!comic.value) return
  const start = Math.max(0, currentIndex.value - 2)
  const end = Math.min(comic.value.files.length, currentIndex.value + 8)

  for (let i = start; i < end; i++) {
    if (!scrollImages.value.has(i)) {
      const url = await getOrFetchImage(i)
      if (url && comic.value === store.pickedComic) {
        scrollImages.value.set(i, url)
      }
    }
  }
}

// ---- Thumbnails
//
// Small copies of the pages, in a cache of their own: the page cache frees the pages that are
// far from the current one, and a thumbnail that shared its image would turn into a broken one
// with them. Only the thumbnails that are in view are made, a few at a time, nearest to the
// current page first.

const THUMBNAIL_HEIGHT = 160
const THUMBNAIL_CONCURRENCY = 3

const thumbnails = ref<Map<number, string>>(new Map())
const thumbnailStripRef = ref<HTMLElement>()
const thumbnailsInView = new Set<number>()
const thumbnailsQueued = new Set<number>()
const thumbnailsLoading = new Set<number>()
const thumbnailsFailed = new Set<number>()
let thumbnailQueue: number[] = []
// bumped when the comic changes, so that a thumbnail that was on its way is thrown away
let thumbnailGeneration = 0
let thumbnailObserver: IntersectionObserver | undefined

function resetThumbnails() {
  thumbnailGeneration++
  for (const url of thumbnails.value.values()) {
    URL.revokeObjectURL(url)
  }
  thumbnails.value.clear()
  thumbnailQueue = []
  thumbnailsQueued.clear()
  thumbnailsLoading.clear()
  thumbnailsFailed.clear()
}

/** `blob` as an image of at most `THUMBNAIL_HEIGHT` pixels in height */
async function scaleDown(blob: Blob): Promise<string | undefined> {
  const source = URL.createObjectURL(blob)
  try {
    const image = new Image()
    image.src = source
    await image.decode()
    const scale = Math.min(1, THUMBNAIL_HEIGHT / image.naturalHeight)
    const canvas = document.createElement('canvas')
    canvas.width = Math.max(1, Math.round(image.naturalWidth * scale))
    canvas.height = Math.max(1, Math.round(image.naturalHeight * scale))
    canvas.getContext('2d')?.drawImage(image, 0, 0, canvas.width, canvas.height)
    const small = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, 'image/jpeg', 0.75))
    return small ? URL.createObjectURL(small) : undefined
  } catch (err) {
    console.error(err)
    return undefined
  } finally {
    URL.revokeObjectURL(source)
  }
}

async function makeThumbnail(c: Comic, index: number): Promise<string | undefined> {
  const blob = await fetchPageBlob(c, index)
  return blob === undefined ? undefined : await scaleDown(blob)
}

function pumpThumbnails() {
  const c = comic.value
  if (c === undefined) return
  // the ones nearest to the page that is shown first
  thumbnailQueue.sort((a, b) => Math.abs(a - currentIndex.value) - Math.abs(b - currentIndex.value))
  while (thumbnailsLoading.size < THUMBNAIL_CONCURRENCY && thumbnailQueue.length > 0) {
    const index = thumbnailQueue.shift()!
    thumbnailsQueued.delete(index)
    // scrolled out of view while it waited, it is asked for again when it comes back
    if (!thumbnailsInView.has(index)) continue
    thumbnailsLoading.add(index)
    const generation = thumbnailGeneration
    makeThumbnail(c, index).then((url) => {
      if (generation !== thumbnailGeneration) {
        if (url) URL.revokeObjectURL(url)
        return
      }
      thumbnailsLoading.delete(index)
      if (url) thumbnails.value.set(index, url)
      else thumbnailsFailed.add(index)
      pumpThumbnails()
    })
  }
}

function wantThumbnail(index: number) {
  if (
    thumbnails.value.has(index) ||
    thumbnailsLoading.has(index) ||
    thumbnailsQueued.has(index) ||
    thumbnailsFailed.has(index)
  ) {
    return
  }
  thumbnailsQueued.add(index)
  thumbnailQueue.push(index)
  pumpThumbnails()
}

function observeThumbnails() {
  thumbnailObserver?.disconnect()
  thumbnailsInView.clear()
  const strip = thumbnailStripRef.value
  if (!strip) return
  thumbnailObserver = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        const index = Number((entry.target as HTMLElement).dataset.index)
        if (entry.isIntersecting) {
          thumbnailsInView.add(index)
          wantThumbnail(index)
        } else {
          thumbnailsInView.delete(index)
        }
      }
    },
    { root: strip, rootMargin: '0px 200px' },
  )
  strip.querySelectorAll('[data-index]').forEach((cell) => thumbnailObserver?.observe(cell))
}

function scrollStripToCurrent() {
  nextTick(() => {
    thumbnailStripRef.value
      ?.querySelector(`[data-index="${currentIndex.value}"]`)
      ?.scrollIntoView({ inline: 'center', block: 'nearest', behavior: 'smooth' })
  })
}

watch(thumbnailStripRef, (strip) => {
  // a template ref is `null` once its element is gone
  if (!strip) {
    thumbnailObserver?.disconnect()
    thumbnailsInView.clear()
    thumbnailQueue = []
    thumbnailsQueued.clear()
    return
  }
  observeThumbnails()
  scrollStripToCurrent()
})

// ---- Slideshow

const SLIDESHOW_SECONDS_KEY = 'reader.slideshowSeconds'
const SLIDESHOW_MIN_SECONDS = 1
const SLIDESHOW_MAX_SECONDS = 60
const SLIDESHOW_DEFAULT_SECONDS = 5

function savedSlideshowSeconds(): number {
  try {
    const seconds = Number(localStorage.getItem(SLIDESHOW_SECONDS_KEY))
    if (Number.isInteger(seconds) && seconds >= SLIDESHOW_MIN_SECONDS && seconds <= SLIDESHOW_MAX_SECONDS) {
      return seconds
    }
  } catch {
    // no storage, the default does
  }
  return SLIDESHOW_DEFAULT_SECONDS
}

const slideshowRunning = ref(false)
const slideshowSeconds = ref(savedSlideshowSeconds())
let slideshowTimer: ReturnType<typeof setTimeout> | undefined

function setSlideshowSeconds(seconds: number | null) {
  if (seconds === null) return
  slideshowSeconds.value = Math.min(SLIDESHOW_MAX_SECONDS, Math.max(SLIDESHOW_MIN_SECONDS, Math.round(seconds)))
  try {
    localStorage.setItem(SLIDESHOW_SECONDS_KEY, String(slideshowSeconds.value))
  } catch {
    // not remembered
  }
}

function stopSlideshow() {
  slideshowRunning.value = false
}

function toggleSlideshow() {
  if (slideshowRunning.value) {
    stopSlideshow()
    return
  }
  if (readMode.value !== 'page' || totalFiles.value < 2) return
  // at the last page there is nothing to go on to, so it starts again from the first
  if (currentIndex.value >= totalFiles.value - 1) currentIndex.value = 0
  slideshowRunning.value = true
}

// The next page comes the chosen time after the current one is shown. It stops at the last
// page, and when the reader leaves the page mode. A page, a click or a key of the reader
// stops it too, those call `stopSlideshow` themselves.
watch([slideshowRunning, currentIndex, loading, slideshowSeconds, readMode], () => {
  clearTimeout(slideshowTimer)
  if (!slideshowRunning.value) return
  if (readMode.value !== 'page') {
    stopSlideshow()
    return
  }
  if (loading.value) return
  if (currentIndex.value >= totalFiles.value - 1) {
    stopSlideshow()
    return
  }
  slideshowTimer = setTimeout(() => {
    currentIndex.value++
  }, slideshowSeconds.value * 1000)
})

// ---- Pages

function goToPage(index: number) {
  if (!comic.value) return
  stopSlideshow()
  const clamped = Math.max(0, Math.min(index, comic.value.files.length - 1))
  currentIndex.value = clamped

  if (readMode.value === 'scroll') {
    startJump()
    nextTick(() => {
      const el = document.getElementById(`scroll-page-${clamped}`)
      el?.scrollIntoView({ behavior: 'smooth', block: 'start' })
    })
  }
}

function prevPage() {
  stopSlideshow()
  if (currentIndex.value > 0) currentIndex.value--
}

function nextPage() {
  stopSlideshow()
  if (comic.value && currentIndex.value < comic.value.files.length - 1) currentIndex.value++
}

function toggleMode() {
  readMode.value = readMode.value === 'page' ? 'scroll' : 'page'
}

// One page for each step of a mouse wheel. A trackpad sends many small steps, they are added up
// and a page is turned for each 60, with a short pause after it so that a flick does not
// skip a dozen pages.
const WHEEL_STEP = 60
const WHEEL_PAUSE_MS = 250
let wheelSum = 0
let wheelPausedUntil = 0

function handleWheel(e: WheelEvent) {
  if (readMode.value !== 'page') return
  e.preventDefault()
  const now = performance.now()
  if (now < wheelPausedUntil) return
  // a wheel that counts in lines instead of pixels
  wheelSum += e.deltaMode === WheelEvent.DOM_DELTA_PIXEL ? e.deltaY : e.deltaY * 40
  if (Math.abs(wheelSum) < WHEEL_STEP) return
  const forward = wheelSum > 0
  wheelSum = 0
  wheelPausedUntil = now + WHEEL_PAUSE_MS
  if (forward) nextPage()
  else prevPage()
}

function handleKeydown(e: KeyboardEvent) {
  if (store.currentTabName !== 'reader') return

  switch (e.key) {
    case 'ArrowRight':
    case 'ArrowDown':
    case ' ':
      e.preventDefault()
      if (readMode.value === 'page') nextPage()
      break
    case 'ArrowLeft':
    case 'ArrowUp':
      e.preventDefault()
      if (readMode.value === 'page') prevPage()
      break
    case 'Home':
      e.preventDefault()
      goToPage(0)
      break
    case 'End':
      e.preventDefault()
      goToPage((comic.value?.files.length ?? 1) - 1)
      break
    case 'PageDown':
      e.preventDefault()
      goToPage(currentIndex.value + 10)
      break
    case 'PageUp':
      e.preventDefault()
      goToPage(currentIndex.value - 10)
      break
    case 't':
      thumbnailsShowing.value = !thumbnailsShowing.value
      break
    case 'f':
      fitWidth.value = !fitWidth.value
      break
    case 'm':
      toggleMode()
      break
    case 's':
      toggleSlideshow()
      break
  }
}

// ---- Current page in scroll mode
//
// The current page is the one at the top of the visible area. An observer watches a thin strip
// along the top edge of the scroll container and keeps the pages that cross it, whatever their
// height, loaded or not. At the very bottom the last page is the current one, it may be too
// short to ever reach the top. While a jump scrolls to its page the pages it passes are not
// taken, the jump has set the current page already.

const JUMP_SETTLE_MS = 150

// the pages that cross the top strip, usually one, two where they meet
const pagesAtTop = new Set<number>()
let scrollPageObserver: IntersectionObserver | undefined
let jumping = false
let jumpTimer: ReturnType<typeof setTimeout> | undefined

/** Pass on the scrolling of a jump, until it has come to rest */
function startJump() {
  jumping = true
  // a jump to where the container already is scrolls nothing, the timer ends it then too
  settleJump()
}

function settleJump() {
  clearTimeout(jumpTimer)
  jumpTimer = setTimeout(() => {
    jumping = false
  }, JUMP_SETTLE_MS)
}

function updateScrollIndex() {
  const container = scrollContainerRef.value
  if (readMode.value !== 'scroll' || jumping || !container || !comic.value) return
  let index: number | undefined
  if (container.scrollTop > 0 && container.scrollTop + container.clientHeight >= container.scrollHeight - 1) {
    index = comic.value.files.length - 1
  } else if (pagesAtTop.size > 0) {
    index = Math.min(...pagesAtTop)
  }
  // between two pages, in the gap, the page stays the one before
  if (index !== undefined && index !== currentIndex.value) currentIndex.value = index
}

function observeScrollPages() {
  scrollPageObserver?.disconnect()
  pagesAtTop.clear()
  const container = scrollContainerRef.value
  if (!container) return
  scrollPageObserver = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        const index = Number((entry.target as HTMLElement).dataset.scrollIndex)
        if (entry.isIntersecting) pagesAtTop.add(index)
        else pagesAtTop.delete(index)
      }
      updateScrollIndex()
    },
    { root: container, rootMargin: '0px 0px -99% 0px' },
  )
  container.querySelectorAll('[data-scroll-index]').forEach((page) => scrollPageObserver?.observe(page))
}

function handleScroll() {
  if (jumping) {
    settleJump()
    return
  }
  updateScrollIndex()
}

watch(scrollContainerRef, (container) => {
  if (!container) {
    scrollPageObserver?.disconnect()
    pagesAtTop.clear()
    return
  }
  observeScrollPages()
})

// another comic has another number of pages
watch(totalFiles, () => nextTick(observeScrollPages))

// A page has its size from the metadata before its image is there, as a placeholder and as an
// image that is not decoded yet. Otherwise the pages below it move when it loads, and with
// them the page a jump is on its way to.
function pageStyle(index: number, placeholder: boolean) {
  const file = comic.value?.files[index]
  if (!file || file.width <= 0 || file.height <= 0) return placeholder ? { width: '100%', height: '600px' } : {}
  return {
    width: fitWidth.value ? '100%' : `${file.width}px`,
    maxWidth: '100%',
    aspectRatio: `${file.width} / ${file.height}`,
  }
}

watch(
  () => store.pickedComic,
  () => {
    stopSlideshow()
    clearCache()
    currentImage.value = undefined
    scrollImages.value.clear()
    currentIndex.value = 0
    if (store.currentTabName === 'reader') {
      if (readMode.value === 'page') loadCurrentImage()
      else loadScrollImages()
    }
  },
)

watch(
  () => store.currentTabName,
  (name) => {
    if (name === 'reader') {
      if (readMode.value === 'page') loadCurrentImage()
      else loadScrollImages()
    } else {
      stopSlideshow()
    }
  },
)

watch(currentIndex, () => {
  if (readMode.value === 'page') loadCurrentImage()
  else loadScrollImages()

  if (thumbnailsShowing.value) scrollStripToCurrent()
})

watch(readMode, () => {
  if (readMode.value === 'scroll') {
    loadScrollImages()
    nextTick(() => goToPage(currentIndex.value))
  }
})

onMounted(() => {
  window.addEventListener('keydown', handleKeydown)
})

onUnmounted(() => {
  window.removeEventListener('keydown', handleKeydown)
  clearTimeout(slideshowTimer)
  clearTimeout(jumpTimer)
  thumbnailObserver?.disconnect()
  scrollPageObserver?.disconnect()
  clearCache()
  currentImage.value = undefined
  scrollImages.value.clear()
})
</script>

<template>
  <div class="h-full flex flex-col bg-gray-900 relative outline-none" tabindex="0">
    <!-- Toolbar -->
    <div class="flex flex-wrap items-center gap-2 px-3 py-1.5 bg-gray-800 text-white text-sm z-10">
      <n-button size="small" :color="buttonColor(false)" text-color="#fff" @click="prevPage" :disabled="currentIndex <= 0">
        <template #icon><n-icon size="18"><PhCaretLeft weight="bold" /></n-icon></template>
      </n-button>
      <span class="min-w-[80px] text-center">{{ currentIndex + 1 }} / {{ totalFiles }}</span>
      <n-button size="small" :color="buttonColor(false)" text-color="#fff" @click="nextPage" :disabled="currentIndex >= totalFiles - 1">
        <template #icon><n-icon size="18"><PhCaretRight weight="bold" /></n-icon></template>
      </n-button>
      <n-slider
        class="flex-1 min-w-[120px] mx-2"
        :value="currentIndex"
        :max="Math.max(0, totalFiles - 1)"
        :step="1"
        :tooltip="false"
        @update:value="goToPage" />
      <n-button size="small" :color="buttonColor(readMode === 'scroll')" text-color="#fff" @click="toggleMode" :title="t('reader_pane.toggle_mode')">
        <template #icon><n-icon size="18"><PhRows v-if="readMode === 'page'" weight="bold" /><PhList v-else weight="bold" /></n-icon></template>
      </n-button>
      <n-button size="small" :color="buttonColor(fitWidth)" text-color="#fff" @click="fitWidth = !fitWidth" :title="t('reader_pane.fit_width')">
        <template #icon><n-icon size="18"><PhArrowsOutSimple weight="bold" /></n-icon></template>
      </n-button>
      <n-button size="small" :color="buttonColor(thumbnailsShowing)" text-color="#fff" @click="thumbnailsShowing = !thumbnailsShowing" :title="t('reader_pane.thumbnails')">
        <template #icon><n-icon size="18"><PhImages weight="bold" /></n-icon></template>
      </n-button>
      <n-button
        size="small"
        :color="buttonColor(slideshowRunning)"
        text-color="#fff"
        :disabled="readMode !== 'page' || totalFiles < 2"
        @click="toggleSlideshow"
        :title="slideshowRunning ? t('reader_pane.slideshow_stop') : t('reader_pane.slideshow_start')">
        <template #icon><n-icon size="18"><PhPause v-if="slideshowRunning" weight="bold" /><PhPlay v-else weight="bold" /></n-icon></template>
      </n-button>
      <n-input-number
        class="w-16 flex-shrink-0"
        size="small"
        :show-button="false"
        :value="slideshowSeconds"
        :min="SLIDESHOW_MIN_SECONDS"
        :max="SLIDESHOW_MAX_SECONDS"
        :step="1"
        :precision="0"
        :title="t('reader_pane.slideshow_seconds')"
        @update:value="setSlideshowSeconds">
        <template #suffix>s</template>
      </n-input-number>
    </div>

    <!-- Page mode -->
    <div v-if="readMode === 'page'" class="flex-1 flex items-center justify-center overflow-hidden p-2" @wheel="handleWheel">
      <n-spin v-if="loading" class="absolute z-10" />
      <img
        v-if="currentImage"
        :src="currentImage"
        :class="['object-contain transition-all duration-200', fitWidth ? 'w-full h-auto' : 'max-w-full max-h-full']"
        @click="nextPage" />
    </div>

    <!-- Scroll mode -->
    <div
      v-else
      ref="scrollContainerRef"
      class="flex-1 overflow-auto"
      @scroll="handleScroll">
      <div class="flex flex-col items-center gap-1 p-2">
        <!-- the wrapper stays when the image replaces the placeholder, the observer keeps it -->
        <div
          v-for="i in totalFiles"
          :key="i - 1"
          :id="`scroll-page-${i - 1}`"
          :data-scroll-index="i - 1"
          class="w-full flex justify-center items-start">
          <img
            v-if="scrollImages.has(i - 1)"
            :src="scrollImages.get(i - 1)"
            :class="['transition-all duration-200', fitWidth ? 'w-full' : 'max-w-full']"
            :style="pageStyle(i - 1, false)"
            :alt="`Page ${i}`" />
          <div v-else class="flex items-center justify-center bg-gray-800" :style="pageStyle(i - 1, true)">
            <n-spin size="small" />
          </div>
        </div>
      </div>
    </div>

    <!-- Thumbnail strip -->
    <Transition name="slide-up">
      <div
        v-if="thumbnailsShowing && comic"
        ref="thumbnailStripRef"
        class="h-24 bg-gray-800 border-t border-gray-700 overflow-x-auto flex gap-1 p-2">
        <div
          v-for="i in totalFiles"
          :key="i - 1"
          :data-index="i - 1"
          :class="[
            'h-full flex-shrink-0 w-16 cursor-pointer rounded overflow-hidden border-2 transition-all duration-150',
            currentIndex === i - 1 ? 'border-blue-500 scale-110' : 'border-transparent hover:border-gray-500',
          ]"
          @click="goToPage(i - 1)">
          <img
            v-if="thumbnails.has(i - 1)"
            :src="thumbnails.get(i - 1)"
            class="w-full h-full object-cover" />
          <div v-else class="w-full h-full flex items-center justify-center text-gray-400 text-xs">
            {{ i }}
          </div>
        </div>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.slide-up-enter-active,
.slide-up-leave-active {
  transition: transform 0.2s ease, opacity 0.2s ease;
}
.slide-up-enter-from,
.slide-up-leave-to {
  transform: translateY(100%);
  opacity: 0;
}
</style>
