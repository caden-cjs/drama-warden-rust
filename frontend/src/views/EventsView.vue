<script setup>
import { computed, h, inject, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { NButton, NTag } from 'naive-ui';
import { errorMessage, eventTitle, formatDate, safePayloadEntries, statusLabel, statusTagType } from '../lib/format.js';

const api = inject('api');
const notify = inject('notify');
const refreshConnection = inject('refreshConnection');
const events = ref([]);
const shows = ref([]);
const loading = ref(true);
const loadingMore = ref(false);
const hasMore = ref(true);
const loadMoreSentinel = ref(null);
const filter = ref('all');
const query = ref('');
const confirmEvent = ref(null);
const processingId = ref(null);
const retryingId = ref(null);
const detailsEvent = ref(null);
const recoveryEvent = ref(null);
const recoveryWorkerConfirmed = ref(false);
const recoveryOutputConfirmed = ref(false);
const recoveryNoOutputConfirmed = ref(false);
const recoveringId = ref(null);
const pageSize = 50;
const filterOptions = [
  { id: 'all', label: '全部' }, { id: 'pending', label: '待处理' },
  { id: 'processing', label: '处理中' }, { id: 'processed', label: '已完成' },
  { id: 'failed', label: '失败' }, { id: 'skipped', label: '已跳过' },
];
let eventOffset = 0;
let requestVersion = 0;
let loadObserver;
const visibleEvents = computed(() => {
  const needle = query.value.trim().toLowerCase();
  return events.value.filter((event) => {
    const matchesStatus = filter.value === 'all' || event.status === filter.value;
    const show = shows.value.find((item) => item.id === event.showId);
    const haystack = [eventTitle(event), event.kind, event.source, show?.name, event.showId]
      .filter(Boolean).join(' ').toLowerCase();
    return matchesStatus && (!needle || haystack.includes(needle));
  });
});
const eventColumns = [
  {
    title: '事件 / 节目', key: 'title', minWidth: 260,
    render: (event) => h('button', {
      class: 'event-name-cell', type: 'button', onClick: () => { detailsEvent.value = event; },
    }, [
      h('span', { class: ['event-row-mark', `status-${event.status}`] }, event.status === 'processed' ? '✓' : event.status === 'failed' ? '!' : '·'),
      h('span', {}, [
        h('strong', {}, eventTitle(event)),
        h('small', {}, `${showName(event)} · ${event.kind} · #${event.id}`),
      ]),
    ]),
  },
  {
    title: '状态', key: 'status', width: 100,
    render: (event) => h(NTag, {
      class: 'app-status-tag', type: statusTagType(event.status), size: 'small', round: true, bordered: false,
    }, { default: () => statusLabel(event.status) }),
  },
  { title: '来源', key: 'source', width: 110, render: (event) => event.source || 'manual' },
  { title: '尝试', key: 'attemptCount', width: 80, render: (event) => event.attemptCount ?? 0 },
  { title: '创建时间', key: 'createdAt', width: 145, render: (event) => formatDate(event.createdAt) },
  {
    title: '操作', key: 'actions', width: 125,
    render: (event) => {
      const button = (label, type, action, disabled = false) => h(NButton, {
        class: `button ${type} button-small`, size: 'small', attrType: 'button', disabled, onClick: action,
      }, { default: () => label });
      if (event.kind === 'new_ep' && event.status === 'pending') {
        return button('处理', 'button-primary', () => { confirmEvent.value = event; });
      }
      if (event.kind === 'new_ep' && (event.status === 'processing' || (event.status === 'failed' && !event.retryable))) {
        return button(event.status === 'processing' ? '中断复核' : '写入复核', 'button-secondary', () => openRecovery(event));
      }
      if (event.status === 'failed' && event.retryable) {
        return button(retryingId.value === event.id ? '检查中…' : '安全重试', 'button-secondary', () => retryEvent(event), retryingId.value === event.id);
      }
      return h(NButton, {
        class: 'icon-button small', quaternary: true, circle: true, attrType: 'button',
        'aria-label': '查看详情', onClick: () => { detailsEvent.value = event; },
      }, { default: () => '›' });
    },
  },
];

async function load(reset = true) {
  if (!reset && (loading.value || loadingMore.value || !hasMore.value)) return;
  if (reset) {
    requestVersion += 1;
    eventOffset = 0;
    events.value = [];
    hasMore.value = true;
    loading.value = true;
  } else {
    loadingMore.value = true;
  }
  const version = requestVersion;
  try {
    const params = new URLSearchParams({ limit: String(pageSize), offset: String(eventOffset) });
    if (filter.value !== 'all') params.set('status', filter.value);
    const requests = [api.request(`/api/events?${params}`)];
    if (!shows.value.length) requests.push(api.request('/api/shows'));
    const [eventResponse, showResponse] = await Promise.all(requests);
    if (version !== requestVersion) return;
    const items = eventResponse.items || [];
    events.value = reset ? items : [...events.value, ...items];
    eventOffset += items.length;
    hasMore.value = items.length === pageSize;
    if (showResponse) shows.value = showResponse.items || [];
  } catch (error) {
    if (version === requestVersion) notify(errorMessage(error), 'error');
  } finally {
    if (version === requestVersion) {
      loading.value = false;
      loadingMore.value = false;
    }
  }
}

watch(filter, () => load());

function showName(event) {
  return shows.value.find((show) => show.id === event.showId)?.name || (event.showId ? `节目 #${event.showId}` : '系统事件');
}

function closeProcessConfirmation() {
  if (processingId.value != null) return;
  confirmEvent.value = null;
}

async function processConfirmed() {
  const event = confirmEvent.value;
  if (!event) return;
  processingId.value = event.id;
  try {
    const result = await api.request(`/api/events/${event.id}/process`, { method: 'POST' });
    const finalStatus = result.event?.status || (result.ok ? 'processed' : 'failed');
    notify(finalStatus === 'processed' ? '事件处理完成。' : `事件状态：${statusLabel(finalStatus)}。`, finalStatus === 'processed' ? 'success' : 'warning');
    confirmEvent.value = null;
    await load();
    await refreshConnection();
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    processingId.value = null;
  }
}

async function retryEvent(event) {
  retryingId.value = event.id;
  try {
    await api.request(`/api/events/${event.id}/retry`, { method: 'POST' });
    notify('事件已回到待处理状态。');
    detailsEvent.value = null;
    await load();
    await refreshConnection();
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    retryingId.value = null;
  }
}

function openRecovery(event) {
  recoveryEvent.value = event;
  recoveryWorkerConfirmed.value = false;
  recoveryOutputConfirmed.value = false;
  recoveryNoOutputConfirmed.value = false;
  detailsEvent.value = null;
}

function closeRecovery() {
  if (recoveringId.value != null) return;
  recoveryEvent.value = null;
  recoveryWorkerConfirmed.value = false;
  recoveryOutputConfirmed.value = false;
  recoveryNoOutputConfirmed.value = false;
}

async function recoverInterrupted(action) {
  const event = recoveryEvent.value;
  if (!event) return;
  if (!recoveryWorkerConfirmed.value) return;
  if (action === 'complete' && !recoveryOutputConfirmed.value) return;
  if (action !== 'complete' && !recoveryNoOutputConfirmed.value) return;
  recoveringId.value = event.id;
  try {
    const result = await api.request(`/api/events/${event.id}/recover`, {
      method: 'POST',
      body: {
        action,
        confirmNoActiveWorker: true,
        ...(action === 'complete' ? { confirmOutputsMatch: true } : {}),
        ...(action !== 'complete' ? { confirmNoOutput: true } : {}),
      },
    });
    notify(result.resolution === 'processed'
      ? '已核对媒体并完成事件。'
      : result.resolution === 'discarded'
        ? '已确认没有本次媒体；旧事件已放弃，可以重新选择分享。'
        : '已确认目标目录没有本次媒体；事件可安全重试。');
    recoveryEvent.value = null;
    recoveryWorkerConfirmed.value = false;
    recoveryOutputConfirmed.value = false;
    recoveryNoOutputConfirmed.value = false;
    await load();
    await refreshConnection();
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    recoveringId.value = null;
  }
}

function summarizePayload(payload = {}) {
  return safePayloadEntries(payload, 8).map(([key, value]) => `${key}: ${value}`);
}

onMounted(() => {
  if (typeof IntersectionObserver !== 'undefined') {
    loadObserver = new IntersectionObserver((entries) => {
      if (!query.value.trim() && entries.some((entry) => entry.isIntersecting)) load(false);
    }, { rootMargin: '240px' });
    if (loadMoreSentinel.value) loadObserver.observe(loadMoreSentinel.value);
  }
  load();
});

onBeforeUnmount(() => loadObserver?.disconnect());
</script>

<template>
  <div class="view-stack">
    <div class="page-intro"><div><h2>追更流程的每一步都有记录。</h2><p>处理待办、查看失败原因，并复核服务中断时留下的写入状态。</p></div><n-button class="button button-secondary" attr-type="button" :disabled="loading" @click="load"><span class="button-icon">⟳</span>{{ loading ? '正在刷新' : '刷新事件' }}</n-button></div>

    <section class="panel toolbar-panel events-toolbar"><div class="search-field"><span aria-hidden="true">⌕</span><n-input v-model:value="query" clearable type="search" placeholder="搜索已加载事件" /></div><div class="filter-tabs"><n-button v-for="option in filterOptions" :key="option.id" attr-type="button" :class="{ selected: filter === option.id }" @click="filter = option.id">{{ option.label }}</n-button></div></section>

    <n-card class="panel event-table-panel" :bordered="false" content-style="padding: 0">
      <div v-if="loading" class="table-loading"><div v-for="index in 5" :key="index" class="skeleton-line"></div></div>
      <div v-else-if="!visibleEvents.length" class="empty-state compact-empty"><div class="empty-illustration">↻</div><strong>{{ events.length ? '没有匹配的事件' : '还没有追更事件' }}</strong><p>{{ events.length ? '调整筛选条件后再试。' : '通过节目详情中的资源预览创建追更事件。' }}</p></div>
      <n-data-table v-else :columns="eventColumns" :data="visibleEvents" :row-key="(event) => event.id" :bordered="false" :single-line="false" :scroll-x="920" />
      <div ref="loadMoreSentinel" class="event-load-more" aria-live="polite">
        <span v-if="loadingMore">正在加载更早的事件…</span>
        <n-button v-else-if="hasMore" class="button button-secondary button-small" attr-type="button" @click="load(false)">加载更早事件</n-button>
        <span v-else>已加载全部 {{ events.length }} 条事件</span>
      </div>
      <div class="table-footer">滚动到底部可继续加载；搜索范围为已加载的事件。</div>
    </n-card>

    <Teleport to="body">
      <div v-if="confirmEvent" class="modal-backdrop" @click.self="closeProcessConfirmation"><section class="modal confirm-modal" role="dialog" aria-modal="true" aria-labelledby="process-confirm-title"><div class="confirm-icon">!</div><span class="modal-kicker">写入操作确认</span><h2 id="process-confirm-title">开始处理这条追更事件？</h2><p>将为「{{ showName(confirmEvent) }}」处理已选中的媒体文件，并写入对应的存储位置。处理失败时若写入结果不确定，需要先检查目标目录再重试。</p><div class="confirm-event-summary"><strong>{{ eventTitle(confirmEvent) }}</strong><small>事件 #{{ confirmEvent.id }} · {{ confirmEvent.payload?.selectedFileCount ?? '已确认文件' }}</small></div><div class="modal-actions"><n-button class="button button-quiet" attr-type="button" :disabled="processingId === confirmEvent.id" @click="closeProcessConfirmation">返回检查</n-button><n-button class="button button-danger" attr-type="button" :disabled="processingId === confirmEvent.id" @click="processConfirmed">{{ processingId === confirmEvent.id ? '正在处理…' : '确认处理' }}</n-button></div></section></div>
      <div v-if="detailsEvent" class="modal-backdrop" @click.self="detailsEvent = null">
        <section class="modal" role="dialog" aria-modal="true" aria-labelledby="event-detail-title">
          <div class="modal-heading">
            <div><span class="modal-kicker">事件详情 / #{{ detailsEvent.id }}</span><h2 id="event-detail-title">{{ eventTitle(detailsEvent) }}</h2><p>{{ showName(detailsEvent) }} · {{ detailsEvent.kind }}</p></div>
            <n-button class="icon-button" attr-type="button" aria-label="关闭" @click="detailsEvent = null">×</n-button>
          </div>
          <div class="event-detail-facts">
            <div><span>状态</span><strong><n-tag class="app-status-tag" size="small" round :bordered="false" :type="statusTagType(detailsEvent.status)">{{ statusLabel(detailsEvent.status) }}</n-tag></strong></div>
            <div><span>来源</span><strong>{{ detailsEvent.source }}</strong></div>
            <div><span>尝试次数</span><strong>{{ detailsEvent.attemptCount }}</strong></div>
            <div><span>创建时间</span><strong>{{ formatDate(detailsEvent.createdAt) }}</strong></div>
          </div>
          <div class="section-rule"><span>事件摘要</span></div>
          <div v-if="summarizePayload(detailsEvent.payload).length" class="payload-list"><div v-for="line in summarizePayload(detailsEvent.payload)" :key="line">{{ line }}</div></div>
          <div v-else class="helper-panel">没有可展示的事件摘要字段。</div>
          <div v-if="detailsEvent.kind === 'new_ep' && detailsEvent.status === 'processing'" class="inline-alert warning-alert">
            <span class="alert-symbol">!</span><div><strong>处理中断复核</strong><p>如果服务重启可能让事件停留在处理中。复核前请确认没有其他应用实例仍在处理这条事件。</p></div>
          </div>
          <div v-else-if="detailsEvent.status === 'failed'" class="inline-alert error-alert">
            <span class="alert-symbol">!</span><div><strong>事件处理失败</strong><p>{{ detailsEvent.retryable ? '服务端确认没有未决写入占位，可以安全重试。' : '写入占位仍保留；先检查目标目录并使用写入复核，确认已保存或确认没有媒体后再继续。' }}</p></div>
          </div>
          <div class="modal-actions">
            <n-button class="button button-quiet" attr-type="button" @click="detailsEvent = null">关闭</n-button>
            <n-button v-if="detailsEvent.kind === 'new_ep' && detailsEvent.status === 'pending'" class="button button-primary" attr-type="button" @click="confirmEvent = detailsEvent; detailsEvent = null">处理事件</n-button>
            <n-button v-else-if="detailsEvent.kind === 'new_ep' && (detailsEvent.status === 'processing' || (detailsEvent.status === 'failed' && !detailsEvent.retryable))" class="button button-secondary" attr-type="button" @click="openRecovery(detailsEvent)">{{ detailsEvent.status === 'processing' ? '中断复核' : '写入复核' }}</n-button>
            <n-button v-else-if="detailsEvent.status === 'failed' && detailsEvent.retryable" class="button button-secondary" attr-type="button" :disabled="retryingId === detailsEvent.id" @click="retryEvent(detailsEvent)">{{ retryingId === detailsEvent.id ? '检查中…' : '安全重试' }}</n-button>
          </div>
        </section>
      </div>
      <div v-if="recoveryEvent" class="modal-backdrop" @click.self="closeRecovery">
        <section class="modal confirm-modal recovery-modal" role="dialog" aria-modal="true" aria-labelledby="recovery-title">
          <div class="confirm-icon">↻</div>
          <span class="modal-kicker">#{{ recoveryEvent.id }} · {{ recoveryEvent.status === 'processing' ? '中断复核' : '写入复核' }}</span>
          <h2 id="recovery-title">核对这次转存停在了哪一步</h2>
          <p>服务不会再次保存分享文件。它会重新读取 Season {{ String(recoveryEvent.payload?.seasonNumber || '').padStart(2, '0') }}：文件名和大小只能筛选候选，不能证明文件来自本次转存。完成操作会继续整理目录、同步资产和刷新索引；确认无输出后才会开放人工重试或放弃旧事件。</p>
          <div class="confirm-event-summary">
            <strong>{{ eventTitle(recoveryEvent) }}</strong>
            <small>{{ showName(recoveryEvent) }} · {{ recoveryEvent.payload?.selectedFiles?.length ?? '所选媒体文件' }} 个文件</small>
          </div>
          <div class="recovery-confirm">
            <n-checkbox v-model:checked="recoveryWorkerConfirmed">我已确认没有其他应用实例正在处理这条事件。</n-checkbox>
          </div>
          <div class="recovery-confirm">
            <n-checkbox v-model:checked="recoveryOutputConfirmed">若选择完成，我确认目标目录中的所选文件确实属于本次事件。</n-checkbox>
          </div>
          <div class="recovery-confirm">
            <n-checkbox v-model:checked="recoveryNoOutputConfirmed">若选择重试或放弃，我已检查目标目录，确认其中没有本次所选媒体。</n-checkbox>
          </div>
          <div class="modal-actions">
            <n-button class="button button-quiet" attr-type="button" :disabled="recoveringId === recoveryEvent.id" @click="closeRecovery">返回</n-button>
            <n-button class="button button-secondary" attr-type="button" :disabled="recoveringId === recoveryEvent.id || !recoveryWorkerConfirmed || !recoveryOutputConfirmed" @click="recoverInterrupted('complete')">{{ recoveringId === recoveryEvent.id ? '正在核对…' : '核对并完成' }}</n-button>
            <n-button class="button button-danger" attr-type="button" :disabled="recoveringId === recoveryEvent.id || !recoveryWorkerConfirmed || !recoveryNoOutputConfirmed" @click="recoverInterrupted('retry_after_review')">确认无媒体并允许重试</n-button>
            <n-button class="button button-danger" attr-type="button" :disabled="recoveringId === recoveryEvent.id || !recoveryWorkerConfirmed || !recoveryNoOutputConfirmed" @click="recoverInterrupted('discard_after_review')">确认无输出并放弃旧事件</n-button>
          </div>
        </section>
      </div>
    </Teleport>
  </div>
</template>
