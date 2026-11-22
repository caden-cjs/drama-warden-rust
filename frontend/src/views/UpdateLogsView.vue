<script setup>
import { computed, h, inject, onMounted, ref } from 'vue';
import { NTag } from 'naive-ui';
import { errorMessage, eventTitle, formatDate, safePayloadEntries, statusLabel, statusTagType } from '../lib/format.js';

const api = inject('api');
const notify = inject('notify');
const records = ref([]);
const shows = ref([]);
const scanRuns = ref([]);
const scanRunsHasMore = ref(false);
const scanRunsLoadingMore = ref(false);
const scanRunsPageSize = 100;
const loading = ref(true);
const loadError = ref('');
const scanLoadError = ref('');
const query = ref('');
const outcome = ref('all');
const selectedRecord = ref(null);

const terminalRecords = computed(() => records.value.filter((event) =>
  ['processed', 'skipped', 'failed'].includes(event.status)));
const successfulCount = computed(() => terminalRecords.value.filter((event) => event.status === 'processed').length);
const failedCount = computed(() => terminalRecords.value.filter((event) => event.status === 'failed').length);
const skippedCount = computed(() => terminalRecords.value.filter((event) => event.status === 'skipped').length);
const visibleRecords = computed(() => {
  const needle = query.value.trim().toLowerCase();
  return terminalRecords.value.filter((event) => {
    const matchesOutcome = outcome.value === 'all' || event.status === outcome.value;
    const show = shows.value.find((item) => item.id === event.showId);
    const text = [eventTitle(event), show?.name, event.kind, event.source, event.showId]
      .filter(Boolean).join(' ').toLowerCase();
    return matchesOutcome && (!needle || text.includes(needle));
  });
});
const recordColumns = [
  { title: '处理时间', key: 'completedAt', width: 145, render: (event) => formatDate(event.completedAt || event.updatedAt) },
  {
    title: '节目 / 事件', key: 'title', minWidth: 280,
    render: (event) => h('button', {
      class: 'update-title update-title-action', type: 'button', onClick: () => { selectedRecord.value = event; },
    }, [
      h('strong', {}, eventTitle(event)),
      h('small', {}, `${showName(event)} · ${event.kind} · ${event.source || 'manual'}`),
    ]),
  },
  {
    title: '结果', key: 'status', width: 100,
    render: (event) => h(NTag, {
      class: 'app-status-tag', type: statusTagType(event.status), size: 'small', round: true, bordered: false,
    }, { default: () => statusLabel(event.status) }),
  },
  { title: '耗时', key: 'elapsed', width: 90, render: (event) => elapsed(event) },
  { title: '尝试', key: 'attemptCount', width: 80, render: (event) => event.attemptCount ?? 0 },
  { title: '', key: 'open', width: 40, render: () => h('span', { class: 'update-chevron', 'aria-hidden': 'true' }, '›') },
];

async function load() {
  loading.value = true;
  const [eventResult, showResult, scanResult] = await Promise.allSettled([
    api.request('/api/events?limit=1000'),
    api.request('/api/shows'),
    api.request(`/api/automation/runs?limit=${scanRunsPageSize}&offset=0`),
  ]);
  if (eventResult.status === 'fulfilled') records.value = eventResult.value.items || [];
  if (showResult.status === 'fulfilled') shows.value = showResult.value.items || [];
  if (scanResult.status === 'fulfilled') {
    scanRuns.value = scanResult.value.items || [];
    scanRunsHasMore.value = scanRuns.value.length === scanRunsPageSize;
  }
  const eventFailure = [eventResult, showResult].find((result) => result.status === 'rejected');
  loadError.value = eventFailure ? errorMessage(eventFailure.reason) : '';
  scanLoadError.value = scanResult.status === 'rejected' ? errorMessage(scanResult.reason) : '';
  const failure = [eventResult, showResult, scanResult].find((result) => result.status === 'rejected');
  if (failure) notify(errorMessage(failure.reason), 'error');
  loading.value = false;
}

async function loadMoreScans() {
  if (scanRunsLoadingMore.value || !scanRunsHasMore.value) return;
  scanRunsLoadingMore.value = true;
  try {
    const response = await api.request(
      `/api/automation/runs?limit=${scanRunsPageSize}&offset=${scanRuns.value.length}`,
    );
    const items = response.items || [];
    scanRuns.value.push(...items);
    scanRunsHasMore.value = items.length === scanRunsPageSize;
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    scanRunsLoadingMore.value = false;
  }
}

function showName(event) {
  return shows.value.find((show) => show.id === event.showId)?.name || (event.showId ? `节目 #${event.showId}` : '系统事件');
}

function elapsed(event) {
  if (!event.startedAt || !event.completedAt) return '—';
  const duration = new Date(event.completedAt).getTime() - new Date(event.startedAt).getTime();
  if (!Number.isFinite(duration) || duration < 0) return '—';
  if (duration < 1000) return '< 1 秒';
  if (duration < 60000) return `${Math.round(duration / 1000)} 秒`;
  return `${Math.floor(duration / 60000)} 分钟`;
}

function scanRunType(run) {
  return run.runType === 'manual' ? '手动预览' : '周期扫描';
}

function scanErrorText(error = {}) {
  const location = [
    error.showId ? `节目 #${error.showId}` : '',
    error.storageId ? `存储 #${error.storageId}` : '',
    error.eventId ? `事件 #${error.eventId}` : '',
  ].filter(Boolean).join(' · ');
  return [error.errorCode || '扫描错误', location].filter(Boolean).join(' · ');
}

function safePayload(payload = {}) {
  return safePayloadEntries(payload, 12);
}

onMounted(load);
</script>

<template>
  <div class="view-stack">
    <div class="page-intro"><div><h2>回看扫描运行和追更处理结果。</h2><p>自动扫描历史与已入队事件分别记录，便于确认定时任务是否运行。</p></div><n-button class="button button-secondary" attr-type="button" :disabled="loading" @click="load"><span class="button-icon">⟳</span>{{ loading ? '正在刷新' : '刷新记录' }}</n-button></div>

    <div class="metric-grid update-metrics">
      <article class="metric-card"><div class="metric-top"><span>已处理</span><span class="metric-icon mint">✓</span></div><div class="metric-value">{{ successfulCount }}<small> 条</small></div><div class="metric-foot">工作流已完成的追更事件</div></article>
      <article class="metric-card"><div class="metric-top"><span>已跳过</span><span class="metric-icon blue">·</span></div><div class="metric-value">{{ skippedCount }}<small> 条</small></div><div class="metric-foot">无需写入或未找到可追集内容</div></article>
      <article class="metric-card"><div class="metric-top"><span>处理失败</span><span class="metric-icon amber">!</span></div><div class="metric-value">{{ failedCount }}<small> 条</small></div><div class="metric-foot">重试前先核对目标目录状态</div></article>
      <article class="metric-card"><div class="metric-top"><span>已加载记录</span><span class="metric-icon lilac">≋</span></div><div class="metric-value">{{ terminalRecords.length }}<small> / {{ records.length }} 条</small></div><div class="metric-foot">最近 1,000 条事件中的终态记录</div></article>
    </div>

    <section class="panel automation-history-panel">
      <div class="panel-heading">
        <div><h3>自动扫描记录</h3><p>记录周期扫描与手动预览的结果</p></div>
        <span class="subtle-count">最近 {{ scanRuns.length }} 次</span>
      </div>
      <div v-if="scanLoadError" class="inline-alert error-alert scan-history-load-error"><span class="alert-symbol">!</span><div><strong>扫描记录未能刷新</strong><p>{{ scanLoadError }}</p><n-button class="text-button" attr-type="button" :disabled="loading" @click="load">重试 <span>→</span></n-button></div></div>
      <div v-if="loading" class="table-loading"><div v-for="index in 3" :key="index" class="skeleton-line"></div></div>
      <div v-else-if="!scanRuns.length && !scanLoadError" class="empty-state compact-empty"><div class="empty-illustration">⟳</div><strong>还没有扫描记录</strong><p>部署此版本后，每轮扫描都会保存在这里。</p></div>
      <div v-else-if="scanRuns.length" class="scan-history-list">
        <article v-for="run in scanRuns" :key="run.id" class="scan-history-item">
          <div class="scan-history-heading">
            <div><strong>{{ scanRunType(run) }}</strong><small>{{ formatDate(run.startedAt) }} · 用时 {{ elapsed(run) }}</small></div>
            <n-tag class="app-status-tag" size="small" round :bordered="false" :type="run.errorCount ? 'error' : 'success'">{{ run.errorCount ? `${run.errorCount} 项错误` : '完成' }}</n-tag>
          </div>
          <div class="scan-history-counts">
            <div><span>节目 / 存储</span><strong>{{ run.showsChecked }} / {{ run.storagesChecked }}</strong></div>
            <div><span>候选 / 可追</span><strong>{{ run.candidatesFound }} / {{ run.eligibleCandidates }}</strong></div>
            <div><span>入队 / 已处理</span><strong>{{ run.eventsQueued }} / {{ run.eventsProcessed }}</strong></div>
            <div><span>处理失败</span><strong :class="{ 'scan-history-failed': run.eventsFailed }">{{ run.eventsFailed }}</strong></div>
          </div>
          <details v-if="run.errors?.length" class="scan-history-errors">
            <summary>查看扫描错误</summary>
            <ul><li v-for="(error, index) in run.errors" :key="`${run.id}-${index}`">{{ scanErrorText(error) }}</li></ul>
          </details>
          <div v-else-if="run.errorCount" class="scan-history-errors">错误详情未保存。</div>
        </article>
      </div>
      <div v-if="scanRunsHasMore" class="scan-history-more"><n-button class="button button-secondary button-small" attr-type="button" :disabled="scanRunsLoadingMore" @click="loadMoreScans">{{ scanRunsLoadingMore ? '正在加载…' : '加载更早扫描' }}</n-button></div>
      <div class="table-footer">每轮扫描都会保存摘要和安全错误码，不保存分享链接或候选载荷。</div>
    </section>

    <section class="panel toolbar-panel events-toolbar"><div class="search-field"><span aria-hidden="true">⌕</span><n-input v-model:value="query" clearable type="search" placeholder="搜索节目、事件类型或来源" /></div><div class="filter-tabs"><n-button v-for="option in [{ id: 'all', label: '全部结果' }, { id: 'processed', label: '已处理' }, { id: 'skipped', label: '已跳过' }, { id: 'failed', label: '失败' }]" :key="option.id" attr-type="button" :class="{ selected: outcome === option.id }" @click="outcome = option.id">{{ option.label }}</n-button></div></section>

    <n-card class="panel event-table-panel" :bordered="false" content-style="padding: 0">
      <div v-if="loadError && records.length" class="inline-alert error-alert"><span class="alert-symbol">!</span><div><strong>更新记录未能刷新</strong><p>{{ loadError }} 当前显示的是上次成功加载的记录。</p><n-button class="text-button" attr-type="button" :disabled="loading" @click="load">重试 <span>→</span></n-button></div></div>
      <div v-if="loading" class="table-loading"><div v-for="index in 5" :key="index" class="skeleton-line"></div></div>
      <div v-else-if="loadError && !records.length" class="empty-state compact-empty"><div class="empty-illustration">!</div><strong>无法加载更新记录</strong><p>{{ loadError }}</p><n-button class="button button-secondary" attr-type="button" @click="load">重试</n-button></div>
      <div v-else-if="!visibleRecords.length" class="empty-state compact-empty"><div class="empty-illustration">≋</div><strong>{{ terminalRecords.length ? '没有符合条件的记录' : '还没有已完成的追更记录' }}</strong><p>{{ terminalRecords.length ? '调整搜索内容或筛选状态。' : '事件完成、跳过或失败后，会出现在这里。' }}</p></div>
      <n-data-table v-else :columns="recordColumns" :data="visibleRecords" :row-key="(event) => event.id" :bordered="false" :single-line="false" :scroll-x="760" />
      <div class="table-footer">此页展示 update_event 工作流事件，不包含插件调用审计日志。</div>
    </n-card>

    <Teleport to="body">
      <div v-if="selectedRecord" class="modal-backdrop" @click.self="selectedRecord = null"><section class="modal" role="dialog" aria-modal="true" aria-labelledby="update-record-title"><div class="modal-heading"><div><span class="modal-kicker">更新记录 / #{{ selectedRecord.id }}</span><h2 id="update-record-title">{{ eventTitle(selectedRecord) }}</h2><p>{{ showName(selectedRecord) }} · {{ selectedRecord.kind }}</p></div><n-button class="icon-button" attr-type="button" aria-label="关闭" @click="selectedRecord = null">×</n-button></div><div class="event-detail-facts"><div><span>结果</span><strong><n-tag class="app-status-tag" size="small" round :bordered="false" :type="statusTagType(selectedRecord.status)">{{ statusLabel(selectedRecord.status) }}</n-tag></strong></div><div><span>来源</span><strong>{{ selectedRecord.source }}</strong></div><div><span>开始时间</span><strong>{{ formatDate(selectedRecord.startedAt) }}</strong></div><div><span>完成时间</span><strong>{{ formatDate(selectedRecord.completedAt) }}</strong></div><div><span>运行时长</span><strong>{{ elapsed(selectedRecord) }}</strong></div><div><span>处理尝试</span><strong>{{ selectedRecord.attemptCount }}</strong></div></div><div class="section-rule"><span>安全摘要</span></div><div v-if="safePayload(selectedRecord.payload).length" class="payload-list"><div v-for="([key, value]) in safePayload(selectedRecord.payload)" :key="key">{{ key }}: {{ value }}</div></div><div v-else class="helper-panel">没有可展示的事件摘要字段。</div><div v-if="selectedRecord.status === 'failed'" class="inline-alert error-alert"><span class="alert-symbol">!</span><div><strong>处理失败</strong><p>若处理已尝试写入媒体，先检查目标目录，再从追更事件页决定是否安全重试。</p></div></div><div class="modal-actions"><n-button class="button button-quiet" attr-type="button" @click="selectedRecord = null">关闭</n-button></div></section></div>
    </Teleport>
  </div>
</template>
