<script setup lang="ts">
import { computed, onMounted, onUnmounted, watch } from 'vue'
import { useDialog, useMessage } from 'naive-ui'
import { PhArrowsClockwise, PhPause, PhPlay, PhTrash } from '@phosphor-icons/vue'
import { useStore } from '../store.ts'
import { useI18n } from '../utils.ts'
import { errorText, relativeTime, Subscription, subscriptionCommands } from '../subscriptions.ts'

const props = defineProps<{
  search: (query: string, pageNum: number) => Promise<void>
}>()

const { t, locale } = useI18n()
const store = useStore()
const message = useMessage()
const dialog = useDialog()

const list = computed(() => store.subscriptions)
const anyChecking = computed(() => list.value?.subscriptions.some((subscription) => subscription.status === 'checking') ?? false)
const anyActive = computed(() => list.value?.subscriptions.some((subscription) => subscription.status !== 'paused') ?? false)

const statusLabels = computed(() => ({
  active: t('following_pane.status_active'),
  paused: t('following_pane.status_paused'),
  error: t('following_pane.status_error'),
  checking: t('following_pane.status_checking'),
}))
const statusTypes = { active: 'success', paused: 'default', error: 'error', checking: 'info' } as const

const ago = (seconds: number) => relativeTime(seconds, locale.value)

// the list is loaded when the tab opens, and kept up to date while it is shown: now and then,
// and every two seconds while a check runs, which is what shows it ending
watch(() => store.currentTabName, (name) => name === 'following' && store.loadSubscriptions(), { immediate: true })
let ticks = 0
let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  timer = setInterval(() => {
    if (store.currentTabName !== 'following') return
    ticks++
    if (anyChecking.value || ticks % 5 === 0) store.loadSubscriptions()
  }, 2000)
})
onUnmounted(() => clearInterval(timer))

/** Run a command, tell what went wrong, and show what it did */
async function run(command: () => Promise<unknown>) {
  try {
    await command()
  } catch (err) {
    message.error(errorText(err))
  }
  await store.loadSubscriptions()
}

const checkNow = (subscription: Subscription) => run(() => subscriptionCommands.check(subscription.id))
const checkAll = () => run(() => subscriptionCommands.checkAll())
const setPaused = (subscription: Subscription, paused: boolean) =>
  run(() => subscriptionCommands.setPaused(subscription.id, paused))

function remove(subscription: Subscription) {
  dialog.warning({
    title: t('following_pane.delete_title'),
    content: t('following_pane.delete_confirm', { query: subscription.query }),
    positiveText: t('delete.button'),
    negativeText: t('common.cancel'),
    onPositiveClick: () => run(() => subscriptionCommands.remove(subscription.id)),
  })
}
</script>

<template>
  <div class="h-full flex flex-col gap-2 p-2 box-border overflow-auto">
    <n-spin v-if="list === undefined" class="m-auto" />
    <template v-else>
      <div class="flex items-center gap-2 text-sm text-gray-600">
        <span v-if="list.intervalHours === null">{{ t('following_pane.automatic_off') }}</span>
        <span v-else>
          {{ t('following_pane.interval', { hours: list.intervalHours }) }}
          <template v-if="list.nextCheckAt !== null">
            · {{ t('following_pane.next_check', { when: ago(list.nextCheckAt) }) }}
          </template>
        </span>
        <n-button class="ml-auto" size="small" :disabled="!anyActive || anyChecking" @click="checkAll">
          <template #icon>
            <n-icon size="18"><PhArrowsClockwise /></n-icon>
          </template>
          {{ t('following_pane.check_all') }}
        </n-button>
      </div>

      <n-empty v-if="list.subscriptions.length === 0" class="mt-8" :description="t('following_pane.empty')" />

      <n-card
        v-for="subscription in list.subscriptions"
        :key="subscription.id"
        content-style="padding: 0.5rem;"
        hoverable>
        <div class="flex items-start gap-2">
          <div class="flex-1 min-w-0 flex flex-col gap-1">
            <div
              class="font-bold text-lg break-all cursor-pointer transition-colors duration-200 hover:text-blue-5"
              :title="subscription.query"
              @click="props.search(subscription.query, 1)">
              {{ subscription.query }}
            </div>
            <div class="text-sm text-gray-500">
              <span v-if="subscription.lastCheckAt !== null">
                {{ t('following_pane.last_check', { when: ago(subscription.lastCheckAt) }) }}
                · {{ t('following_pane.new_found', { count: subscription.lastNew }) }}
              </span>
              <span v-else>{{ t('following_pane.never_checked') }}</span>
              · {{ t('following_pane.matches', { count: subscription.matches }) }}
            </div>
            <n-alert v-if="subscription.lastError !== null" type="error" size="small" :show-icon="false">
              {{ subscription.lastError }}
            </n-alert>
          </div>
          <n-tag :type="statusTypes[subscription.status]" size="small" round>
            <template v-if="subscription.status === 'checking'" #icon>
              <n-spin :size="12" />
            </template>
            {{ statusLabels[subscription.status] }}
          </n-tag>
        </div>
        <div class="flex gap-1 mt-2 justify-end">
          <n-button size="tiny" :disabled="subscription.status === 'checking'" @click="checkNow(subscription)">
            <template #icon>
              <n-icon size="16"><PhArrowsClockwise /></n-icon>
            </template>
            {{ t('following_pane.check_now') }}
          </n-button>
          <n-button
            v-if="subscription.status === 'paused'"
            size="tiny"
            @click="setPaused(subscription, false)">
            <template #icon>
              <n-icon size="16"><PhPlay /></n-icon>
            </template>
            {{ t('following_pane.resume') }}
          </n-button>
          <n-button v-else size="tiny" @click="setPaused(subscription, true)">
            <template #icon>
              <n-icon size="16"><PhPause /></n-icon>
            </template>
            {{ t('following_pane.pause') }}
          </n-button>
          <n-button size="tiny" type="error" @click="remove(subscription)">
            <template #icon>
              <n-icon size="16"><PhTrash /></n-icon>
            </template>
            {{ t('delete.button') }}
          </n-button>
        </div>
      </n-card>
    </template>
  </div>
</template>
