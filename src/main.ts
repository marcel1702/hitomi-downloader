import { createApp } from 'vue'
import { createPinia } from 'pinia'
import App from './App.vue'
import 'virtual:uno.css'
import { createI18n } from 'vue-i18n'
import { DEFAULT_LOCALE, locales, resolveLocale } from './locales'
import type { MessageSchema, SupportedLocales } from './locales'

export type { MessageSchema, SupportedLocales }

/** Create and configure the Vue I18n instance used for translations */
export const i18n = createI18n<[MessageSchema], SupportedLocales>({
  // Provisional until the saved language is read from the config on startup,
  // so a fresh install opens in the language of the system where one fits
  locale: resolveLocale(navigator.language),
  // Fall back to English for anything a locale does not define
  fallbackLocale: DEFAULT_LOCALE,
  // Allow using $t and the other i18n properties directly in components
  globalInjection: true,
  // Disable legacy mode so the Vue 3 Composition API can be used
  legacy: false,
  // The imported language packs
  messages: locales,
})

// Pinia instance for Vue state management
const pinia = createPinia()
// The Vue application instance
const app = createApp(App)

// Use Pinia and I18n, then mount the app on the '#app' DOM node
app.use(pinia).use(i18n).mount('#app')
