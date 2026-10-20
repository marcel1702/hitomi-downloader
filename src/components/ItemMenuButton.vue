<script setup lang="ts">
// The ⋮ of a comic or a download on a touch screen: it opens the menu a right click opens with a
// mouse, where a long press does not (Safari on iOS). Without touch it is not there.
import { PhDotsThreeVertical } from '@phosphor-icons/vue'
import { useDevice } from '../composables/useDevice.ts'
import { useI18n } from '../utils.ts'

const { t } = useI18n()
const { touch } = useDevice()

const emit = defineEmits<{
  // where the menu opens, below the button, like the position of a right click
  open: [position: { clientX: number; clientY: number }]
}>()

function onClick(event: MouseEvent) {
  const button = (event.currentTarget as HTMLElement).getBoundingClientRect()
  emit('open', { clientX: button.left, clientY: button.bottom })
}
</script>

<template>
  <!-- the taps stay here, the selection area around would take them as a selection -->
  <div
    v-if="touch"
    class="absolute top-2 right-2 z-10 bg-white/70 rounded"
    @mousedown.stop
    @touchstart.stop
    @pointerdown.stop>
    <n-button quaternary circle :title="t('app_content.menu')" @click.stop="onClick">
      <template #icon>
        <n-icon size="22">
          <PhDotsThreeVertical />
        </n-icon>
      </template>
    </n-button>
  </div>
</template>
