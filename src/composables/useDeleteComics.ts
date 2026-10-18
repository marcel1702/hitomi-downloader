import { useDialog, useMessage } from 'naive-ui'
import { Comic, commands } from '../bindings.ts'
import { useStore } from '../store.ts'
import { useI18n } from '../utils.ts'

/**
 * Deleting downloaded comics from the library, desktop app and web version alike
 *
 * Every delete asks first. Only the comic in the library goes, exports stay.
 */
export function useDeleteComics() {
  const { t } = useI18n()
  const dialog = useDialog()
  const message = useMessage()
  const store = useStore()

  /** Ask, then delete `comics` one after the other. Resolves with the ones that are gone. */
  function deleteComics(comics: Comic[]): Promise<Comic[]> {
    return new Promise((resolve) => {
      if (comics.length === 0) {
        resolve([])
        return
      }
      dialog.warning({
        title: t('delete.title'),
        content:
          comics.length === 1
            ? t('delete.confirm_one', { title: comics[0].title })
            : t('delete.confirm_many', { count: comics.length }),
        positiveText: t('delete.button'),
        negativeText: t('common.cancel'),
        // a promise keeps the dialog open, with a loading button, until the comics are gone
        onPositiveClick: async () => resolve(await deleteAll(comics)),
        onNegativeClick: () => resolve([]),
        onClose: () => resolve([]),
        onMaskClick: () => resolve([]),
      })
    })
  }

  async function deleteAll(comics: Comic[]): Promise<Comic[]> {
    const deleted: Comic[] = []
    for (const comic of comics) {
      const result = await commands.deleteComic(comic)
      if (result.status === 'ok') {
        deleted.push(comic)
      } else {
        console.error(result.error)
        message.error(t('delete.failed', { title: comic.title }))
      }
    }
    if (deleted.length > 0) {
      message.success(t('delete.deleted', { count: deleted.length }))
      forget(deleted)
    }
    return deleted
  }

  /** What the app still shows of a comic that is not in the library any more */
  function forget(comics: Comic[]) {
    const ids = new Set(comics.map((comic) => comic.id))
    const notDownloaded = (comic: Comic) => {
      if (ids.has(comic.id)) {
        comic.isDownloaded = false
        comic.comicDownloadDir = null
      }
    }
    // changed in place, a new object would make the comic tab and the reader load it again
    if (store.pickedComic !== undefined) notDownloaded(store.pickedComic)
    store.searchResult?.comics.forEach(notDownloaded)
  }

  return { deleteComics }
}
