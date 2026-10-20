<script setup lang="ts">
// The Kavita of the web version: where it is, its API key and the library it scans after
// downloads. Saved with its own button, the API key is a secret and only goes to the server.
import { computed, onMounted, ref } from 'vue'
import { useMessage } from 'naive-ui'
import { useI18n } from '../utils.ts'
import { KavitaLibrary, kavitaCommands, KavitaSettingsArgs } from '../kavita.ts'
import { errorText } from '../subscriptions.ts'

const { t } = useI18n()
const message = useMessage()

const url = ref('')
// empty keeps the key the server has, if it has one
const apiKey = ref('')
const hasApiKey = ref(false)
// the × was clicked: the next save takes the key away
const apiKeyCleared = ref(false)
const libraryId = ref<number | null>(null)
const libraries = ref<KavitaLibrary[]>([])
const testing = ref(false)
const saving = ref(false)
// what the last test found, shown below the fields
const testResult = ref<{ ok: boolean; text: string }>()

const libraryOptions = computed(() => {
  const options = libraries.value.map((library) => ({ label: `${library.name} (${library.id})`, value: library.id }))
  // a library that was saved shows before a test has listed them
  if (libraryId.value !== null && !libraries.value.some((library) => library.id === libraryId.value)) {
    options.unshift({ label: t('kavita.library_id', { id: libraryId.value }), value: libraryId.value })
  }
  return options
})

function args(): KavitaSettingsArgs {
  let key: string | null = apiKey.value
  if (key === '') key = apiKeyCleared.value ? '' : null
  return { url: url.value, apiKey: key, libraryId: libraryId.value }
}

onMounted(async () => {
  try {
    const settings = await kavitaCommands.get()
    url.value = settings.url
    hasApiKey.value = settings.hasApiKey
    libraryId.value = settings.libraryId
  } catch (err) {
    console.error(err)
  }
})

async function test() {
  testing.value = true
  testResult.value = undefined
  try {
    libraries.value = await kavitaCommands.test(args())
    const chosen = libraries.value.find((library) => library.id === libraryId.value)
    if (libraries.value.length === 1 && libraryId.value === null) libraryId.value = libraries.value[0].id
    testResult.value = {
      ok: true,
      text:
        chosen !== undefined
          ? t('kavita.connected_library', { name: chosen.name })
          : t('kavita.connected', { count: libraries.value.length }),
    }
  } catch (err) {
    libraries.value = []
    testResult.value = { ok: false, text: errorText(err) }
  } finally {
    testing.value = false
  }
}

async function save() {
  saving.value = true
  try {
    const settings = await kavitaCommands.save(args())
    hasApiKey.value = settings.hasApiKey
    url.value = settings.url
    apiKey.value = ''
    apiKeyCleared.value = false
    message.success(t('kavita.saved'))
  } catch (err) {
    message.error(errorText(err))
  } finally {
    saving.value = false
  }
}

function clearApiKey() {
  apiKey.value = ''
  hasApiKey.value = false
  apiKeyCleared.value = true
}
</script>

<template>
  <div class="flex flex-col gap-row-2">
    <div class="font-semibold mt-2">Kavita</div>
    <div class="text-xs text-gray-500">{{ t('kavita.explanation') }}</div>
    <n-input-group>
      <n-input-group-label size="small">{{ t('kavita.url') }}</n-input-group-label>
      <n-input v-model:value="url" size="small" placeholder="http://kavita:5000" />
    </n-input-group>
    <n-input-group>
      <n-input-group-label size="small">{{ t('kavita.api_key') }}</n-input-group-label>
      <n-input
        v-model:value="apiKey"
        size="small"
        type="password"
        :placeholder="hasApiKey ? t('kavita.api_key_saved') : t('kavita.api_key_placeholder')" />
      <n-button v-if="hasApiKey" size="small" :title="t('kavita.api_key_clear')" @click="clearApiKey">×</n-button>
    </n-input-group>
    <n-input-group>
      <n-input-group-label size="small">{{ t('kavita.library') }}</n-input-group-label>
      <n-select
        v-model:value="libraryId"
        size="small"
        clearable
        :options="libraryOptions"
        :placeholder="t('kavita.library_placeholder')" />
    </n-input-group>
    <div v-if="testResult" :class="['text-xs', testResult.ok ? 'text-green-600' : 'text-red-600']">
      {{ testResult.text }}
    </div>
    <div class="flex gap-2 justify-end">
      <n-button size="small" :loading="testing" :disabled="url.trim() === ''" @click="test">
        {{ t('kavita.test') }}
      </n-button>
      <n-button size="small" type="primary" :loading="saving" @click="save">{{ t('kavita.save') }}</n-button>
    </div>
  </div>
</template>
