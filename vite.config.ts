import { defineConfig, type Plugin } from 'vite'
// @ts-expect-error node has no types in this project, as for process below
import { readFileSync } from 'node:fs'
import vue from '@vitejs/plugin-vue'
import AutoImport from 'unplugin-auto-import/vite'
import Components from 'unplugin-vue-components/vite'
import { NaiveUiResolver } from 'unplugin-vue-components/resolvers'
import UnoCSS from 'unocss/vite'
import vueJsx from '@vitejs/plugin-vue-jsx'
import vueDevTools from 'vite-plugin-vue-devtools'

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST
// @ts-expect-error process is a nodejs global
const webApi = process.env.HITOMI_WEB_API ?? 'http://127.0.0.1:8080'

// The web version (`--mode web`) runs the same app in a browser against hitomi-server:
// these stand-ins take the place of the Tauri modules, see `src/web`
const webAliases = [
  { find: /^@tauri-apps\/api$/, replacement: '/src/web/tauri-api.ts' },
  { find: /^@tauri-apps\/api\/core$/, replacement: '/src/web/tauri-core.ts' },
  { find: /^@tauri-apps\/api\/event$/, replacement: '/src/web/tauri-event.ts' },
  { find: /^@tauri-apps\/api\/webviewWindow$/, replacement: '/src/web/tauri-webview-window.ts' },
  { find: /^@tauri-apps\/api\/window$/, replacement: '/src/web/tauri-window.ts' },
  { find: /^@tauri-apps\/api\/app$/, replacement: '/src/web/tauri-app.ts' },
  { find: /^@tauri-apps\/api\/path$/, replacement: '/src/web/tauri-path.ts' },
  { find: /^@tauri-apps\/plugin-dialog$/, replacement: '/src/web/tauri-dialog.ts' },
]

const rustDirs = ['**/src-tauri/**', '**/crates/**', '**/target/**']

// The icon of the app for the browser: favicon, home screen and web app manifest. They are the
// icons of the desktop app in `src-tauri/icons`, served under `/icons` and copied there by the
// build, so that there is one set of them.
const appIcons: Record<string, string> = {
  'icon-32.png': '32x32.png',
  'icon-64.png': '64x64.png',
  'icon-256.png': '128x128@2x.png',
  'icon-512.png': 'icon.png',
}
const appIconsDir = 'src-tauri/icons'

function appIconsPlugin(): Plugin {
  return {
    name: 'app-icons',
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const name = (request as { url?: string }).url?.match(/^\/icons\/([^/?]+)/)?.[1]
        const source = name === undefined ? undefined : appIcons[name]
        if (source === undefined) return next()
        response.setHeader('Content-Type', 'image/png')
        response.end(readFileSync(`${appIconsDir}/${source}`))
      })
    },
    generateBundle() {
      for (const name of Object.keys(appIcons)) {
        this.emitFile({ type: 'asset', fileName: `icons/${name}`, source: readFileSync(`${appIconsDir}/${appIcons[name]}`) })
      }
    },
  }
}

// https://vitejs.dev/config/
export default defineConfig(async ({ mode }) => ({
  resolve: {
    alias: mode === 'web' ? webAliases : [],
  },
  build: {
    // kept apart from the desktop build, the server serves this directory
    outDir: mode === 'web' ? 'dist-web' : 'dist',
  },
  plugins: [
    vue(),
    UnoCSS(),
    vueJsx({}),
    vueDevTools(),
    AutoImport({
      imports: [
        'vue',
        {
          'naive-ui': ['useDialog', 'useMessage', 'useNotification', 'useLoadingBar'],
        },
      ],
    }),
    Components({
      resolvers: [NaiveUiResolver()],
    }),
    appIconsPlugin(),
  ],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent vite from obscuring rust errors
  clearScreen: false,
  server:
    mode === 'web'
      ? {
          // `pnpm dev:web` passes the API through to a hitomi-server running on its own
          port: 5006,
          proxy: { '/api': webApi },
          watch: { ignored: rustDirs },
        }
      : {
          // 2. tauri expects a fixed port, fail if that port is not available
          port: 5005,
          strictPort: true,
          host: host || false,
          hmr: host
            ? {
                protocol: 'ws',
                host,
                port: 1421,
              }
            : undefined,
          watch: {
            // 3. tell vite to ignore watching the Rust side: the crates and the build output of the workspace
            ignored: rustDirs,
          },
        },
}))
