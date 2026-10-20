<script setup lang="ts">
// This starter template is using Vue 3 <script setup> SFCs
// Check out https://vuejs.org/api/sfc-script-setup.html#script-setup
import { computed } from 'vue'
import AppContent from './AppContent.vue'
import { GlobalThemeOverrides } from 'naive-ui'
import { useDevice } from './composables/useDevice.ts'

const { touch } = useDevice()

const themeOverrides = computed<GlobalThemeOverrides>(() => ({
  common: {
    primaryColor: '#6275C0FF',
    primaryColorHover: '#8998CCFF',
    primaryColorPressed: '#465499FF',
    primaryColorSuppl: '#8998CCFF',
    // for a finger: the controls nearly all use the small size, the theme makes them bigger at once
    ...(touch.value ? { heightTiny: '32px', heightSmall: '40px', heightMedium: '40px', fontSizeSmall: '15px' } : {}),
  },
  // the tags of a card are many and stay small, a tap on one still hits it
  ...(touch.value ? { Tag: { heightSmall: '28px', heightMedium: '28px' } } : {}),
}))
</script>

<template>
  <n-config-provider :theme-overrides="themeOverrides">
    <n-modal-provider>
      <n-notification-provider placement="bottom-right">
        <n-message-provider>
          <n-dialog-provider>
            <app-content />
          </n-dialog-provider>
        </n-message-provider>
      </n-notification-provider>
    </n-modal-provider>
  </n-config-provider>
</template>
<style>
.selection-container .selected {
  @apply bg-[rgb(204,232,255)];
}

.selection-area {
  @apply bg-[rgba(46,115,252,0.5)];
}

/* a swipe sideways turns a page of the reader, it does not go back in the history of the tab */
html[data-touch],
html[data-touch] body {
  overscroll-behavior-x: none;
}

/* a row of buttons that fits next to each other on a wide screen breaks into lines on a phone */
[data-narrow] .wrap-on-narrow {
  flex-wrap: wrap;
}

/* on a phone the dialogs of the app take the whole screen, the content scrolls inside them */
[data-narrow] .n-dialog.app-dialog {
  width: 100vw !important;
  max-width: 100vw;
  min-height: 100dvh;
  margin: 0;
  border-radius: 0;
  box-sizing: border-box;
}
</style>
