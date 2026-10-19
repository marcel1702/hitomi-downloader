import { commands } from '../bindings.ts'

/**
 * Open a file or folder in the file manager of the desktop app
 *
 * The web version has no file manager, its folders are on the server. It shows them in the
 * settings and the path of a comic in a dialog of the comic tab instead.
 */
export function useRevealPath() {
  async function revealPath(path: string) {
    const result = await commands.showPathInFileManager(path)
    if (result.status === 'error') {
      console.error(result.error)
    }
  }

  return { revealPath }
}
