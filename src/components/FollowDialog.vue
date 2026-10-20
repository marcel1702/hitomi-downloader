<script setup lang="ts">
import { ref, watch } from 'vue'
import { useMessage } from 'naive-ui'
import { useStore } from '../store.ts'
import { useI18n } from '../utils.ts'
import { errorText, FirstRun, SubscriptionPreview, subscriptionCommands } from '../subscriptions.ts'

const props = defineProps<{ query: string }>()
const showing = defineModel<boolean>('showing', { required: true })

const { t } = useI18n()
const store = useStore()
const message = useMessage()

const preview = ref<SubscriptionPreview>()
const loading = ref(false)
const loadError = ref<string>()
const firstRun = ref<FirstRun>('fromNow')
const creating = ref(false)
const createError = ref<string>()

async function load() {
  preview.value = undefined
  loadError.value = undefined
  createError.value = undefined
  firstRun.value = 'fromNow'
  loading.value = true
  const query = props.query
  try {
    const result = await subscriptionCommands.preview(query)
    // the dialog was closed and opened for another search while this one was on its way
    if (query === props.query) preview.value = result
  } catch (err) {
    if (query === props.query) loadError.value = errorText(err)
  } finally {
    loading.value = false
  }
}

watch(
  showing,
  (open) => {
    if (open) load()
  },
  { immediate: true },
)

async function follow() {
  if (preview.value === undefined || creating.value) return
  creating.value = true
  createError.value = undefined
  try {
    await subscriptionCommands.create(preview.value.query, firstRun.value)
    message.success(t('follow_dialog.followed', { query: preview.value.query }))
    showing.value = false
    store.loadSubscriptions()
  } catch (err) {
    createError.value = errorText(err)
  } finally {
    creating.value = false
  }
}
</script>

<template>
  <n-modal v-model:show="showing">
    <n-dialog class="app-dialog" :show-icon="false" :title="t('follow_dialog.title')" @close="showing = false">
      <div class="flex flex-col gap-3">
        <div class="font-mono bg-gray-100 rounded px-2 py-1 break-all">{{ preview?.query ?? query }}</div>

        <div v-if="loading" class="flex items-center gap-2 text-gray-500">
          <n-spin :size="16" />
          {{ t('follow_dialog.loading') }}
        </div>
        <n-alert v-else-if="loadError !== undefined" type="error" :show-icon="false">
          {{ loadError }}
          <n-button class="ml-2" size="tiny" @click="load">{{ t('follow_dialog.retry') }}</n-button>
        </n-alert>

        <template v-else-if="preview !== undefined">
          <div>{{ t('follow_dialog.matches', { matches: preview.matches, inLibrary: preview.inLibrary }) }}</div>
          <n-alert v-if="preview.alreadyFollowed" type="warning" :show-icon="false">
            {{ t('follow_dialog.already_followed') }}
          </n-alert>
          <n-radio-group v-model:value="firstRun" class="flex flex-col gap-2" :disabled="preview.alreadyFollowed">
            <n-radio value="fromNow">{{ t('follow_dialog.from_now') }}</n-radio>
            <n-radio value="everything" :disabled="!preview.canDownloadAll">
              {{ t('follow_dialog.everything', { count: preview.toDownload }) }}
            </n-radio>
          </n-radio-group>
          <div v-if="!preview.canDownloadAll" class="text-gray-500 text-sm">
            {{ t('follow_dialog.too_many', { limit: preview.limit }) }}
          </div>
        </template>

        <n-alert v-if="createError !== undefined" type="error" :show-icon="false">{{ createError }}</n-alert>
      </div>
      <template #action>
        <n-button size="small" @click="showing = false">{{ t('common.cancel') }}</n-button>
        <n-button
          size="small"
          type="primary"
          :loading="creating"
          :disabled="preview === undefined || preview.alreadyFollowed"
          @click="follow">
          {{ t('follow_dialog.follow') }}
        </n-button>
      </template>
    </n-dialog>
  </n-modal>
</template>
