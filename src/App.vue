<script setup lang="ts">
// This starter template is using Vue 3 <script setup> SFCs
// Check out https://vuejs.org/api/sfc-script-setup.html#script-setup
import AppContent from './AppContent.vue'
import { GlobalThemeOverrides } from 'naive-ui'

const themeOverrides: GlobalThemeOverrides = {
  common: {
    primaryColor: '#6275C0FF',
    primaryColorHover: '#8998CCFF',
    primaryColorPressed: '#465499FF',
    primaryColorSuppl: '#8998CCFF',
  },
}
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
