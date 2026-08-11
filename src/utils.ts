import * as i18n from 'vue-i18n'
import type { MessageSchema, SupportedLocales } from './locales'

export function useI18n() {
  return i18n.useI18n<{ message: MessageSchema }, SupportedLocales>()
}
