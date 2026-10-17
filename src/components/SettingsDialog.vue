<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { useStore } from '../store'
import { computed, ref } from 'vue'
import { path } from '@tauri-apps/api'
import { appDataDir } from '@tauri-apps/api/path'
import { commands } from '../bindings.ts'

const { t } = useI18n()

const store = useStore()

const showing = defineModel<boolean>('showing', { required: true })

const proxyHost = ref<string>(store.config?.proxy.host ?? '')
const proxyUsername = ref<string>(store.config?.proxy.username ?? '')
const proxyPassword = ref<string>(store.config?.proxy.password ?? '')
const dirFmt = ref<string>(store.config?.dirFmt ?? '')

const disableProxyFields = computed(() => store.config?.proxy.enabled !== true)
const proxyScheme = computed(() => (store.config?.proxy.remoteDns === false ? 'socks5://' : 'socks5h://'))

function commitProxyCredentials() {
  if (store.config === undefined) {
    return
  }
  // username and password are only valid together, so they are committed in one go
  store.config.proxy.username = proxyUsername.value
  store.config.proxy.password = proxyPassword.value
}

function updateProxyPort(port: number | null) {
  // a cleared field has no port, keep the last one instead of saving an invalid config
  if (store.config !== undefined && port !== null) {
    store.config.proxy.port = port
  }
}

// kept in step with COMIC_CONCURRENCY_RANGE and IMG_CONCURRENCY_RANGE in config.rs
const MAX_COMIC_CONCURRENCY = 8
const MAX_IMG_CONCURRENCY = 16

function updateComicConcurrency(value: number | null) {
  // a cleared field keeps the last limit, the same as the proxy port
  if (store.config !== undefined && value !== null) {
    store.config.comicConcurrency = value
  }
}

function updateImgConcurrency(value: number | null) {
  if (store.config !== undefined && value !== null) {
    store.config.imgConcurrency = value
  }
}

async function showConfigInFileManager() {
  const configName = 'config.json'
  const configPath = await path.join(await appDataDir(), configName)
  const result = await commands.showPathInFileManager(configPath)
  if (result.status === 'error') {
    console.error(result.error)
  }
}
</script>

<template>
  <n-modal v-model:show="showing" v-if="store.config !== undefined">
    <n-dialog :showIcon="false" :title="t('settings_dialog.name')" @close="showing = false">
      <div class="flex flex-col gap-row-2">
        <n-radio-group class="flex gap-2" v-model:value="store.config.downloadFormat">
          <span>{{ t('settings_dialog.download_format') }}</span>
          <n-radio value="Webp">webp</n-radio>
          <n-tooltip placement="top" trigger="hover">
            {{ t('settings_dialog.avif_warning') }}
            <template #trigger>
              <n-radio value="Avif">avif</n-radio>
            </template>
          </n-tooltip>
        </n-radio-group>
        <div class="flex gap-2">
          <n-input-group>
            <n-input-group-label size="small">{{ t('settings_dialog.comic_concurrency') }}</n-input-group-label>
            <n-input-number
              :value="store.config.comicConcurrency"
              @update:value="updateComicConcurrency"
              size="small"
              :min="1"
              :max="MAX_COMIC_CONCURRENCY"
              :parse="(x: string) => parseInt(x)" />
          </n-input-group>
          <n-input-group>
            <n-input-group-label size="small">{{ t('settings_dialog.img_concurrency') }}</n-input-group-label>
            <n-input-number
              :value="store.config.imgConcurrency"
              @update:value="updateImgConcurrency"
              size="small"
              :min="1"
              :max="MAX_IMG_CONCURRENCY"
              :parse="(x: string) => parseInt(x)" />
          </n-input-group>
        </div>
        <div class="flex items-center gap-2">
          <span>{{ t('settings_dialog.socks5_proxy') }}</span>
          <n-switch v-model:value="store.config.proxy.enabled" size="small" />
          <n-tooltip placement="top" trigger="hover">
            {{ t('settings_dialog.proxy_remote_dns_tip') }}
            <template #trigger>
              <n-checkbox v-model:checked="store.config.proxy.remoteDns" :disabled="disableProxyFields" size="small">
                {{ t('settings_dialog.proxy_remote_dns') }}
              </n-checkbox>
            </template>
          </n-tooltip>
        </div>
        <n-input-group>
          <n-input-group-label size="small">{{ proxyScheme }}</n-input-group-label>
          <n-input
            :disabled="disableProxyFields"
            v-model:value="proxyHost"
            size="small"
            :placeholder="t('settings_dialog.proxy_host')"
            @blur="store.config.proxy.host = proxyHost"
            @keydown.enter="store.config.proxy.host = proxyHost" />
          <n-input-group-label size="small">:</n-input-group-label>
          <n-input-number
            :disabled="disableProxyFields"
            :value="store.config.proxy.port"
            @update:value="updateProxyPort"
            size="small"
            :min="1"
            :max="65535"
            :placeholder="t('settings_dialog.proxy_port')"
            :parse="(x: string) => parseInt(x)" />
        </n-input-group>
        <n-input-group>
          <n-input
            :disabled="disableProxyFields"
            v-model:value="proxyUsername"
            size="small"
            :placeholder="t('settings_dialog.proxy_username')"
            :input-props="{ autocomplete: 'off' }"
            @blur="commitProxyCredentials"
            @keydown.enter="commitProxyCredentials" />
          <n-input
            :disabled="disableProxyFields"
            v-model:value="proxyPassword"
            type="password"
            show-password-on="click"
            size="small"
            :placeholder="t('settings_dialog.proxy_password')"
            :input-props="{ autocomplete: 'new-password' }"
            @blur="commitProxyCredentials"
            @keydown.enter="commitProxyCredentials" />
        </n-input-group>
        <n-tooltip placement="top" trigger="hover" width="580">
          <i18n-t keypath="settings_dialog.directory_format.directory_level_tips" tag="div" scope="global">
            <template v-slot:slash>
              <span class="rounded bg-gray-500 px-1 text-white">/</span>
            </template>
          </i18n-t>
          <div class="font-semibold mt-2">{{ t('settings_dialog.directory_format.available_fields') }}</div>
          <div>
            <div>
              <span class="rounded bg-gray-500 px-1">id</span>
              <span class="ml-2">{{ t('settings_dialog.directory_format.id') }}</span>
            </div>
            <div>
              <span class="rounded bg-gray-500 px-1">title</span>
              <span class="ml-2">{{ t('settings_dialog.directory_format.title') }}</span>
            </div>
            <div>
              <span class="rounded bg-gray-500 px-1">type</span>
              <span class="ml-2">{{ t('settings_dialog.directory_format.type') }}</span>
            </div>
            <div>
              <span class="rounded bg-gray-500 px-1">artists</span>
              <span class="ml-2">{{ t('settings_dialog.directory_format.artists') }}</span>
            </div>
            <div>
              <span class="rounded bg-gray-500 px-1">language</span>
              <span class="ml-2">{{ t('settings_dialog.directory_format.language') }}</span>
            </div>
            <div>
              <span class="rounded bg-gray-500 px-1">language_localname</span>
              <span class="ml-2">{{ t('settings_dialog.directory_format.language_localname') }}</span>
            </div>
            <div>
              <span class="rounded bg-gray-500 px-1">tags</span>
              <span class="ml-2">{{ t('common.tag') }}</span>
            </div>
          </div>
          <div class="font-semibold mt-2">{{ t('settings_dialog.directory_format.for_example') }}</div>
          <div class="bg-gray-200 rounded-md p-1 text-black w-fit">
            {type}/{artists}/[{artists}] {title}({id}) - {language}({language_localname})
          </div>
          <div class="font-semibold">{{ t('settings_dialog.directory_format.directory_result') }}</div>
          <div class="flex gap-1 text-black">
            <div class="bg-gray-200 rounded-md px-2 w-fit">doujinshi</div>
            <span class="rounded bg-gray-500 px-1 text-white">/</span>
            <div class="bg-gray-200 rounded-md px-2 w-fit">mameroku</div>
            <span class="rounded bg-gray-500 px-1 text-white">/</span>
            <div class="bg-gray-200 rounded-md px-2 w-fit">[mameroku] Soushi Souai.(2829145) - german(Deutsch)</div>
          </div>
          <template #trigger>
            <n-input-group class="box-border">
              <n-input-group-label size="small">{{ t('settings_dialog.directory_format.name') }}</n-input-group-label>
              <n-input
                v-model:value="dirFmt"
                size="small"
                @blur="store.config.dirFmt = dirFmt"
                @keydown.enter="store.config.dirFmt = dirFmt" />
            </n-input-group>
          </template>
        </n-tooltip>
        <n-button class="ml-auto mt-2" size="small" @click="showConfigInFileManager">
          {{ t('settings_dialog.open_config_directory') }}
        </n-button>
      </div>
    </n-dialog>
  </n-modal>
</template>
