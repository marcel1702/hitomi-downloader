<script setup lang="ts">
// The notifications of the web version: the Apprise server, the Apprise URLs it sends to, what
// is told and whether the messages name titles. Saved with its own button, like Kavita.
import { onMounted, ref } from 'vue'
import { useMessage } from 'naive-ui'
import { useI18n } from '../utils.ts'
import { notifyCommands, NotifySettingsArgs } from '../notify.ts'
import { errorText } from '../subscriptions.ts'

const { t } = useI18n()
const message = useMessage()

const url = ref('')
// empty keeps the Apprise URLs the server has, if it has any
const urlsInput = ref('')
const urlCount = ref(0)
// the × was clicked: the next save takes the Apprise URLs away
const urlsCleared = ref(false)
const onCompleted = ref(true)
const onFailed = ref(true)
const onFollowed = ref(true)
const censored = ref(false)
const testing = ref(false)
const saving = ref(false)
const testResult = ref<{ ok: boolean; text: string }>()

function args(): NotifySettingsArgs {
  const lines = urlsInput.value
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '')
  let urls: string[] | null = lines
  if (lines.length === 0) urls = urlsCleared.value ? [] : null
  return {
    url: url.value,
    urls,
    onCompleted: onCompleted.value,
    onFailed: onFailed.value,
    onFollowed: onFollowed.value,
    censored: censored.value,
  }
}

onMounted(async () => {
  try {
    const settings = await notifyCommands.get()
    url.value = settings.url
    urlCount.value = settings.urlCount
    onCompleted.value = settings.onCompleted
    onFailed.value = settings.onFailed
    onFollowed.value = settings.onFollowed
    censored.value = settings.censored
  } catch (err) {
    console.error(err)
  }
})

async function test() {
  testing.value = true
  testResult.value = undefined
  try {
    await notifyCommands.test(args())
    testResult.value = { ok: true, text: t('notify.test_sent') }
  } catch (err) {
    testResult.value = { ok: false, text: errorText(err) }
  } finally {
    testing.value = false
  }
}

async function save() {
  saving.value = true
  try {
    const settings = await notifyCommands.save(args())
    url.value = settings.url
    urlCount.value = settings.urlCount
    urlsInput.value = ''
    urlsCleared.value = false
    message.success(t('notify.saved'))
  } catch (err) {
    message.error(errorText(err))
  } finally {
    saving.value = false
  }
}

function clearUrls() {
  urlsInput.value = ''
  urlCount.value = 0
  urlsCleared.value = true
}
</script>

<template>
  <div class="flex flex-col gap-row-2">
    <div class="font-semibold mt-2">{{ t('notify.title') }}</div>
    <div class="text-xs text-gray-500">{{ t('notify.explanation') }}</div>
    <n-input-group>
      <n-input-group-label size="small">{{ t('notify.server') }}</n-input-group-label>
      <n-input v-model:value="url" size="small" placeholder="http://apprise:8000" />
    </n-input-group>
    <div class="flex gap-1 items-start">
      <n-input
        v-model:value="urlsInput"
        class="font-mono"
        type="textarea"
        size="small"
        :autosize="{ minRows: 2, maxRows: 6 }"
        :placeholder="urlCount > 0 ? t('notify.urls_saved', { count: urlCount }) : t('notify.urls_placeholder')" />
      <n-button v-if="urlCount > 0" size="small" :title="t('notify.urls_clear')" @click="clearUrls">×</n-button>
    </div>
    <div class="flex flex-col gap-1 text-sm">
      <n-checkbox v-model:checked="onCompleted">{{ t('notify.on_completed') }}</n-checkbox>
      <n-checkbox v-model:checked="onFailed">{{ t('notify.on_failed') }}</n-checkbox>
      <n-checkbox v-model:checked="onFollowed">{{ t('notify.on_followed') }}</n-checkbox>
      <n-checkbox v-model:checked="censored">{{ t('notify.censored') }}</n-checkbox>
    </div>
    <div v-if="testResult" :class="['text-xs', testResult.ok ? 'text-green-600' : 'text-red-600']">
      {{ testResult.text }}
    </div>
    <div class="flex gap-2 justify-end">
      <n-button size="small" :loading="testing" :disabled="url.trim() === ''" @click="test">
        {{ t('notify.test') }}
      </n-button>
      <n-button size="small" type="primary" :loading="saving" @click="save">{{ t('notify.save') }}</n-button>
    </div>
  </div>
</template>
