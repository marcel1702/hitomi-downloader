// What kind of screen the web version is on, asked from the browser and not guessed from its name
//
// - narrow: the window is narrower than the two columns need, the layout becomes one column
// - touch: a finger is the primary input, the controls get bigger and work without a mouse
//
// Both follow the browser live, a turned tablet or a resized window switches on the spot. The
// desktop app keeps its layout, for it both are always false.
import { readonly, ref } from 'vue'
import { isWeb } from '../platform.ts'

/** The width the two columns are made for, below it the layout is one column */
export const NARROW_BELOW_PX = 800

const narrow = ref(false)
const touch = ref(false)

function follow(query: string, target: typeof narrow, attribute: string) {
  const media = window.matchMedia(query)
  const update = () => {
    target.value = media.matches
    document.documentElement.toggleAttribute(attribute, media.matches)
  }
  update()
  media.addEventListener('change', update)
}

if (isWeb && typeof window !== 'undefined' && typeof window.matchMedia === 'function') {
  // a fraction below the breakpoint, so a window of exactly 800 keeps both columns
  follow(`(max-width: ${NARROW_BELOW_PX - 0.02}px)`, narrow, 'data-narrow')
  follow('(pointer: coarse)', touch, 'data-touch')
}

export function useDevice() {
  return { narrow: readonly(narrow), touch: readonly(touch) }
}
