import { ref } from 'vue'
import { SelectionArea, SelectionEvent } from '@viselect/vue'

/**
 * The options of every selection area: dragging a frame works with the mouse only. On a touch
 * screen a finger scrolls the list, a tap still selects one item.
 */
export const SELECTION_OPTIONS = {
  selectables: '.selectable',
  features: { deselectOnBlur: true, touch: false },
}

export function useMultiSelect() {
  const selectedIds = ref<Set<number>>(new Set())
  const selectionAreaRef = ref<InstanceType<typeof SelectionArea>>()
  const selectableRefs = ref<HTMLDivElement[]>([])

  function extractIds(elements: Element[]): number[] {
    return elements
      .map((element) => element.getAttribute('data-key'))
      .filter(Boolean)
      .map(Number)
  }

  function updateSelectedIds({
    store: {
      changed: { added, removed },
    },
  }: SelectionEvent) {
    extractIds(added).forEach((id) => selectedIds.value.add(id))
    extractIds(removed).forEach((id) => selectedIds.value.delete(id))
  }

  function unselectAll({ event, selection }: SelectionEvent) {
    if (!event?.ctrlKey && !event?.metaKey) {
      selection.clearSelection()
      selectedIds.value.clear()
    }
  }

  function onContextMenu(id: number) {
    if (selectedIds.value.has(id)) {
      return
    }
    selectedIds.value.clear()
    selectedIds.value.add(id)
  }

  // the selection area is the element that scrolls through the comic cards
  function scrollToTop() {
    const container = selectionAreaRef.value?.$el as HTMLElement | undefined
    container?.scrollTo({ top: 0, behavior: 'instant' })
  }

  // The cards are replaced when the page changes. The selection area still holds the old ones,
  // so forget them without an event, which would take their ids out of the selection.
  function forgetElements() {
    selectionAreaRef.value?.selection?.clearSelection(true, true)
  }

  function clearSelection() {
    forgetElements()
    selectedIds.value = new Set()
  }

  function toggleSelection(id: number, checked: boolean) {
    const newSelectedIds = new Set(selectedIds.value)
    if (checked) {
      newSelectedIds.add(id)
    } else {
      newSelectedIds.delete(id)
    }
    selectedIds.value = newSelectedIds
  }

  return {
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
  }
}
