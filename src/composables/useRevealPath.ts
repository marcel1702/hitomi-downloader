import { computed } from 'vue'
import { useMessage } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { PhCopy, PhFolderOpen } from '@phosphor-icons/vue'
import { commands } from '../bindings.ts'
import { copyText, isWeb } from '../platform.ts'

/**
 * Show where a file or folder is
 *
 * The desktop app opens it in the file manager. The files of the web version are on the
 * server, there the path is copied to the clipboard, or shown when the browser refuses that.
 */
export function useRevealPath() {
  const message = useMessage()
  const { t } = useI18n()

  async function revealPath(path: string) {
    if (!isWeb) {
      const result = await commands.showPathInFileManager(path)
      if (result.status === 'error') {
        console.error(result.error)
      }
      return
    }

    if (await copyText(path)) {
      message.success(t('common.path_copied', { path }))
    } else {
      message.info(t('common.path_on_server', { path }), { duration: 10000, closable: true })
    }
  }

  const revealLabel = computed(() => (isWeb ? t('common.copy_path') : t('common.open_directory')))
  const RevealIcon = isWeb ? PhCopy : PhFolderOpen

  return { revealPath, revealLabel, RevealIcon }
}
