<script setup lang="ts">
import { useStore } from '../store.ts'
import { Comic, commands } from '../bindings.ts'
import { isWeb, pageUrl, setAppFullscreen } from '../platform.ts'
import { ref, watch, onMounted, onUnmounted, computed, nextTick } from 'vue'
import {
  PhCaretLeft,
  PhCaretRight,
  PhList,
  PhRows,
  PhImages,
  PhPlay,
  PhPause,
  PhCornersOut,
  PhCornersIn,
  PhFrameCorners,
  PhArrowsOutLineHorizontal,
  PhArrowsOutLineVertical,
  PhNumberSquareOne,
  PhQuestion,
} from '@phosphor-icons/vue'
import { useI18n } from '../utils.ts'

const { t } = useI18n()
const store = useStore()

type ReadMode = 'page' | 'scroll'
/** How a page is fitted into the reader: whole, to the width, to the height, or as it is */
type FitMode = 'page' | 'width' | 'height' | 'original'

const FIT_MODES: FitMode[] = ['page', 'width', 'height', 'original']

const currentIndex = ref(0)
const currentImage = ref<string | undefined>(undefined)
const loading = ref(false)
const readMode = ref<ReadMode>('page')
const thumbnailsShowing = ref(false)
const fitMode = ref<FitMode>('page')
const pageContainerRef = ref<HTMLElement>()
const scrollContainerRef = ref<HTMLElement>()
const scrollImages = ref<Map<number, string>>(new Map())
// the notice after the last page, and the list of the shortcuts
const endShowing = ref(false)
const helpShowing = ref(false)

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

// The next page comes the chosen time after the current one is shown. On the last page the
// end notice comes instead and the slideshow stops, as it stops when the reader leaves the page
// mode. A page, a click or a key of the reader stops it too, those call `stopSlideshow`
// themselves.
watch([slideshowRunning, currentIndex, loading, slideshowSeconds, readMode], () => {
  clearTimeout(slideshowTimer)
  if (!slideshowRunning.value) return
  if (readMode.value !== 'page') {
    stopSlideshow()
    return
  }
  if (loading.value) return
  slideshowTimer = setTimeout(() => {
    if (currentIndex.value < totalFiles.value - 1) {
      currentIndex.value++
      return
    }
    stopSlideshow()
    endShowing.value = true
  }, slideshowSeconds.value * 1000)
})

// ---- Pages

function goToPage(index: number) {
  if (!comic.value) return
  stopSlideshow()
  endShowing.value = false
  const clamped = Math.max(0, Math.min(index, comic.value.files.length - 1))
  currentIndex.value = clamped
  if (readMode.value === 'scroll') scrollToPage(clamped, 'smooth')
}

/** Scroll the scroll mode to page `index`, without taking the pages on the way as the current one */
function scrollToPage(index: number, behavior: 'smooth' | 'instant') {
  startJump(index)
  nextTick(() => {
    document.getElementById(`scroll-page-${index}`)?.scrollIntoView({ behavior, block: 'start' })
  })
}

function prevPage() {
  stopSlideshow()
  // the end notice comes after the last page, back from it is the last page again
  if (endShowing.value) {
    endShowing.value = false
    return
  }
  // the buttons of the toolbar, the scroll mode has to scroll there
  if (readMode.value === 'scroll') goToPage(currentIndex.value - 1)
  else if (currentIndex.value > 0) currentIndex.value--
}

function nextPage() {
  stopSlideshow()
  if (!comic.value) return
  if (endShowing.value) {
    endShowing.value = false
    return
  }
  // the end notice belongs to the page mode, the scroll mode simply ends
  if (readMode.value === 'scroll') goToPage(currentIndex.value + 1)
  else if (currentIndex.value < comic.value.files.length - 1) currentIndex.value++
  else if (comic.value.files.length > 0) endShowing.value = true
}

function toggleMode() {
  endShowing.value = false
  readMode.value = readMode.value === 'page' ? 'scroll' : 'page'
}

function nextFitMode() {
  fitMode.value = FIT_MODES[(FIT_MODES.indexOf(fitMode.value) + 1) % FIT_MODES.length]
}

const fitModeIcons = {
  page: PhFrameCorners,
  width: PhArrowsOutLineHorizontal,
  height: PhArrowsOutLineVertical,
  original: PhNumberSquareOne,
}

/**
 * The image of the page mode, fitted as `fitMode` says. `m-auto` centers it without cutting off what overflows.
 *
 * The whole page fills the reader as far as it can without being cut, made bigger as well as smaller:
 * the image takes the whole area and `object-contain` fits the page into it.
 */
const pageImageClass = computed(() => {
  switch (fitMode.value) {
    case 'page':
      return 'w-full h-full'
    case 'width':
      return 'w-full h-auto flex-shrink-0'
    case 'height':
      return 'h-full w-auto max-w-none flex-shrink-0'
    case 'original':
      return 'max-w-none flex-shrink-0'
  }
  return ''
})

// A page that is bigger than the reader is scrolled from its top, wherever the one before was
watch(currentImage, () => pageContainerRef.value?.scrollTo(0, 0))

/** A click on the page: the left third goes back, the right third forward, the middle shows or hides the toolbar */
function handlePageClick(e: MouseEvent) {
  const area = (e.currentTarget as HTMLElement).getBoundingClientRect()
  const x = (e.clientX - area.left) / area.width
  if (x < 1 / 3) prevPage()
  else if (x > 2 / 3) nextPage()
  else toggleToolbar()
}

// One page for each step of a mouse wheel. A trackpad sends many small steps, they are added up
// and a page is turned for each 60, with a short pause after it so that a flick does not
// skip a dozen pages. A page that is higher than the reader is scrolled through first, the
// wheel turns the page once it is at the end.
const WHEEL_STEP = 60
const WHEEL_PAUSE_MS = 250
let wheelSum = 0
let wheelPausedUntil = 0

function handleWheel(e: WheelEvent) {
  if (readMode.value !== 'page') return
  // sideways, through a page that is wider than the reader
  if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) return
  const now = performance.now()
  const area = e.currentTarget as HTMLElement
  const canScroll = e.deltaY > 0
    ? area.scrollTop + area.clientHeight < area.scrollHeight - 1
    : area.scrollTop > 0
  if (canScroll) {
    // the browser scrolls, and the end of the page is no reason to turn it right away
    wheelSum = 0
    wheelPausedUntil = now + WHEEL_PAUSE_MS
    return
  }
  e.preventDefault()
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

// ---- Fullscreen
//
// Only the reader goes fullscreen: it moves out of the tabs, over the whole app, and the window
// goes fullscreen with it, the browser tab through the Fullscreen API and the desktop app
// through its Tauri window. In fullscreen the toolbar lies over the page and hides with the
// pointer when the mouse rests.

const IDLE_MS = 2000

const fullscreen = ref(false)
// hidden with a click in the middle of the page
const toolbarHidden = ref(false)
// the mouse rested in fullscreen
const idle = ref(false)
let idleTimer: ReturnType<typeof setTimeout> | undefined
let pointerOnToolbar = false

const toolbarShowing = computed(() => !toolbarHidden.value && !(fullscreen.value && idle.value))

function wake() {
  idle.value = false
  clearTimeout(idleTimer)
  if (fullscreen.value && !pointerOnToolbar) {
    idleTimer = setTimeout(() => {
      idle.value = true
    }, IDLE_MS)
  }
}

function setPointerOnToolbar(on: boolean) {
  pointerOnToolbar = on
  wake()
}

function toggleToolbar() {
  if (toolbarShowing.value) {
    toolbarHidden.value = true
  } else {
    toolbarHidden.value = false
    wake()
  }
}

async function setFullscreen(on: boolean) {
  if (fullscreen.value === on) return
  // the reader moves in the page, which loses where its scroll mode was, see the watcher below
  startJump(currentIndex.value)
  fullscreen.value = on
  wake()
  try {
    // called before anything is awaited, the browser only allows it during the key press or click
    await setAppFullscreen(on)
  } catch (err) {
    // the reader still lies over the app, it is only the window that stays as it is
    console.error(err)
  }
}

function toggleFullscreen() {
  setFullscreen(!fullscreen.value)
}

watch(fullscreen, () => {
  if (readMode.value === 'scroll') scrollToPage(currentIndex.value, 'instant')
})

// Esc in the browser leaves its fullscreen by itself, without a key event for the page
function handleFullscreenChange() {
  if (document.fullscreenElement || !fullscreen.value) return
  helpShowing.value = false
  endShowing.value = false
  setFullscreen(false)
}

// ---- Overlays

function backToComic() {
  endShowing.value = false
  setFullscreen(false)
  store.currentTabName = 'comic'
}

const shortcuts = computed(() => [
  { keys: '→  ↓  Space', action: t('reader_pane.shortcut_next') },
  { keys: '←  ↑', action: t('reader_pane.shortcut_prev') },
  { keys: 'Home  End', action: t('reader_pane.shortcut_first_last') },
  { keys: 'PageUp  PageDown', action: t('reader_pane.shortcut_ten_pages') },
  { keys: 'F', action: t('reader_pane.shortcut_fullscreen') },
  { keys: 'W', action: t('reader_pane.shortcut_fit') },
  { keys: 'M', action: t('reader_pane.shortcut_mode') },
  { keys: 'T', action: t('reader_pane.shortcut_thumbnails') },
  { keys: 'S', action: t('reader_pane.shortcut_slideshow') },
  { keys: 'Esc', action: t('reader_pane.shortcut_escape') },
  { keys: '?', action: t('reader_pane.shortcut_help') },
  // the mouse, no keys
  { keys: t('reader_pane.click_sides'), action: t('reader_pane.click_sides_action'), mouse: true },
  { keys: t('reader_pane.click_middle'), action: t('reader_pane.click_middle_action'), mouse: true },
  { keys: t('reader_pane.wheel'), action: t('reader_pane.wheel_action'), mouse: true },
])

// ---- Keys

function handleKeydown(e: KeyboardEvent) {
  if (store.currentTabName !== 'reader') return
  // the shortcuts of the browser or the system, and typing in the seconds of the slideshow
  if (e.ctrlKey || e.metaKey || e.altKey) return
  if (e.target instanceof HTMLElement && e.target.closest('input, textarea, [contenteditable="true"]')) return

  // a letter is the same key with Shift or Caps Lock
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key
  switch (key) {
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
    case 'Escape':
      if (helpShowing.value || endShowing.value) {
        helpShowing.value = false
        endShowing.value = false
      } else {
        setFullscreen(false)
      }
      break
    case '?':
      helpShowing.value = !helpShowing.value
      break
    case 't':
      thumbnailsShowing.value = !thumbnailsShowing.value
      break
    case 'f':
      toggleFullscreen()
      break
    case 'w':
      nextFitMode()
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
// taken, the jump has set the current page already. Where the jump comes to rest without its
// page in view, because the wheel took over on the way, the page is the one at the top again.

const JUMP_SETTLE_MS = 150

// the pages that cross the top strip, usually one, two where they meet
const pagesAtTop = new Set<number>()
let scrollPageObserver: IntersectionObserver | undefined
let jumping = false
let jumpTarget = 0
let jumpTimer: ReturnType<typeof setTimeout> | undefined

/** Pass on the scrolling of a jump to page `index`, until it has come to rest */
function startJump(index: number) {
  jumping = true
  jumpTarget = index
  // a jump to where the container already is scrolls nothing, the timer ends it then too
  settleJump()
}

function settleJump() {
  clearTimeout(jumpTimer)
  jumpTimer = setTimeout(() => {
    jumping = false
    const container = scrollContainerRef.value
    const target = document.getElementById(`scroll-page-${jumpTarget}`)
    if (!container || !target) return
    const view = container.getBoundingClientRect()
    const page = target.getBoundingClientRect()
    if (page.bottom <= view.top || page.top >= view.bottom) updateScrollIndex()
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

// The height a page fits into in scroll mode, the visible area without its padding
const SCROLL_PADDING = 16
const scrollViewHeight = ref(0)
const scrollResizeObserver = new ResizeObserver((entries) => {
  for (const entry of entries) scrollViewHeight.value = Math.max(1, entry.contentRect.height - SCROLL_PADDING)
})
watch(scrollContainerRef, (container, old) => {
  if (old) scrollResizeObserver.unobserve(old)
  if (container) scrollResizeObserver.observe(container)
})

// Another fit, or another size of the reader, gives every page another height. The page that
// was at the top stays at the top.
watch([fitMode, scrollViewHeight], () => {
  if (readMode.value === 'scroll') scrollToPage(currentIndex.value, 'instant')
})

// A page has its size from the metadata before its image is there, as a placeholder and as an
// image that is not decoded yet. Otherwise the pages below it move when it loads, and with
// them the page a jump is on its way to.
function pageStyle(index: number, placeholder: boolean): Record<string, string> {
  const height = scrollViewHeight.value
  const file = comic.value?.files[index]
  if (!file || file.width <= 0 || file.height <= 0) {
    if (placeholder) return { width: '100%', height: '600px' }
    switch (fitMode.value) {
      case 'page':
        return { width: '100%', height: `${height}px`, objectFit: 'contain' }
      case 'width':
        return { width: '100%' }
      case 'height':
        return { height: `${height}px`, maxWidth: 'none' }
      case 'original':
        return { maxWidth: 'none' }
    }
  }
  const ratio = file.width / file.height
  const size = { aspectRatio: `${file.width} / ${file.height}` }
  switch (fitMode.value) {
    case 'page':
      // as in page mode, as big as it fits, bigger or smaller than it is
      return { ...size, width: `${height * ratio}px`, maxWidth: '100%' }
    case 'width':
      return { ...size, width: '100%' }
    case 'height':
      return { ...size, width: `${height * ratio}px`, maxWidth: 'none' }
    case 'original':
      return { ...size, width: `${file.width}px`, maxWidth: 'none' }
  }
  return size
}

watch(
  () => store.pickedComic,
  () => {
    stopSlideshow()
    endShowing.value = false
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
      // a reader in fullscreen would lie over the tab that was chosen
      setFullscreen(false)
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
  document.addEventListener('fullscreenchange', handleFullscreenChange)
})

onUnmounted(() => {
  window.removeEventListener('keydown', handleKeydown)
  document.removeEventListener('fullscreenchange', handleFullscreenChange)
  clearTimeout(slideshowTimer)
  clearTimeout(jumpTimer)
  clearTimeout(idleTimer)
  scrollResizeObserver.disconnect()
  thumbnailObserver?.disconnect()
  scrollPageObserver?.disconnect()
  clearCache()
  currentImage.value = undefined
  scrollImages.value.clear()
})
</script>

<template>
  <!-- in fullscreen the reader leaves the tabs and lies over the whole app -->
  <Teleport to="body" :disabled="!fullscreen">
    <div
      :class="[
        'flex flex-col bg-gray-900 outline-none',
        fullscreen ? 'fixed inset-0 z-[3000]' : 'h-full relative',
        fullscreen && idle ? 'cursor-none' : '',
      ]"
      tabindex="0"
      @mousemove="wake">
      <!-- Toolbar -->
      <!-- in fullscreen it lies over the page and fades, so that the page does not move -->
      <div
        v-show="toolbarShowing || fullscreen"
        :class="[
          'flex flex-wrap items-center gap-2 px-3 py-1.5 bg-gray-800 text-white text-sm z-10',
          fullscreen ? 'absolute top-0 inset-x-0 bg-opacity-90 transition-opacity duration-200' : '',
          fullscreen && !toolbarShowing ? 'opacity-0 pointer-events-none' : '',
        ]"
        @mouseenter="setPointerOnToolbar(true)"
        @mouseleave="setPointerOnToolbar(false)">
        <n-button size="small" :color="buttonColor(false)" text-color="#fff" @click="prevPage" :disabled="currentIndex <= 0 && !endShowing">
          <template #icon><n-icon size="18"><PhCaretLeft weight="bold" /></n-icon></template>
        </n-button>
        <span class="min-w-[80px] text-center">{{ currentIndex + 1 }} / {{ totalFiles }}</span>
        <n-button
          size="small"
          :color="buttonColor(false)"
          text-color="#fff"
          @click="nextPage"
          :disabled="readMode === 'page' ? totalFiles === 0 : currentIndex >= totalFiles - 1">
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
        <n-button size="small" :color="buttonColor(fitMode !== 'page')" text-color="#fff" @click="nextFitMode" :title="t(`reader_pane.fit_${fitMode}`)">
          <template #icon><n-icon size="18"><component :is="fitModeIcons[fitMode]" weight="bold" /></n-icon></template>
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
        <n-button
          size="small"
          :color="buttonColor(fullscreen)"
          text-color="#fff"
          @click="toggleFullscreen"
          :title="fullscreen ? t('reader_pane.exit_fullscreen') : t('reader_pane.fullscreen')">
          <template #icon><n-icon size="18"><PhCornersIn v-if="fullscreen" weight="bold" /><PhCornersOut v-else weight="bold" /></n-icon></template>
        </n-button>
        <n-button size="small" :color="buttonColor(helpShowing)" text-color="#fff" @click="helpShowing = !helpShowing" :title="t('reader_pane.shortcuts')">
          <template #icon><n-icon size="18"><PhQuestion weight="bold" /></n-icon></template>
        </n-button>
      </div>

      <!-- Page mode: the left third goes back, the right third forward, the middle shows or hides the toolbar -->
      <div
        v-if="readMode === 'page'"
        ref="pageContainerRef"
        :class="['flex-1 flex relative p-2', fitMode === 'page' ? 'overflow-hidden' : 'overflow-auto']"
        @click="handlePageClick"
        @wheel="handleWheel">
        <div v-if="loading" class="absolute inset-0 flex items-center justify-center pointer-events-none z-10">
          <n-spin />
        </div>
        <img
          v-if="currentImage"
          :src="currentImage"
          :class="['m-auto object-contain transition-all duration-200', pageImageClass]" />
      </div>

      <!-- Scroll mode -->
      <div
        v-else
        ref="scrollContainerRef"
        class="flex-1 overflow-auto"
        @scroll="handleScroll">
        <div class="flex flex-col gap-1 p-2">
          <!-- the wrapper stays when the image replaces the placeholder, the observer keeps it.
               `mx-auto` centers a page without cutting off the part that is wider than the reader. -->
          <div
            v-for="i in totalFiles"
            :key="i - 1"
            :id="`scroll-page-${i - 1}`"
            :data-scroll-index="i - 1"
            class="w-full flex items-start">
            <img
              v-if="scrollImages.has(i - 1)"
              :src="scrollImages.get(i - 1)"
              class="mx-auto flex-shrink-0"
              :style="pageStyle(i - 1, false)"
              :alt="`Page ${i}`" />
            <div v-else class="mx-auto flex-shrink-0 flex items-center justify-center bg-gray-800" :style="pageStyle(i - 1, true)">
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

      <!-- End of the comic -->
      <div
        v-if="endShowing"
        class="absolute inset-0 z-20 flex items-center justify-center bg-black bg-opacity-50"
        data-reader-overlay="end"
        @click.self="endShowing = false">
        <div class="flex flex-col items-center gap-3 rounded-lg bg-gray-800 px-6 py-4 text-white shadow-lg">
          <div class="text-lg font-bold">{{ t('reader_pane.end') }}</div>
          <div class="max-w-80 text-center text-sm text-gray-300 break-words">{{ comic?.title }}</div>
          <n-button type="primary" size="small" @click="backToComic">{{ t('reader_pane.back_to_comic') }}</n-button>
        </div>
      </div>

      <!-- Shortcuts -->
      <div
        v-if="helpShowing"
        class="absolute inset-0 z-20 flex items-center justify-center bg-black bg-opacity-50"
        data-reader-overlay="help"
        @click.self="helpShowing = false">
        <div class="max-h-[90%] overflow-auto rounded-lg bg-gray-800 px-5 py-4 text-white text-sm shadow-lg">
          <div class="mb-2 text-base font-bold">{{ t('reader_pane.shortcuts_title') }}</div>
          <table>
            <tbody>
              <tr v-for="shortcut in shortcuts" :key="shortcut.keys">
                <td :class="['py-0.5 pr-4 align-top whitespace-nowrap text-gray-300', shortcut.mouse ? '' : 'font-mono']">{{ shortcut.keys }}</td>
                <td class="py-0.5">{{ shortcut.action }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </div>
  </Teleport>
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
