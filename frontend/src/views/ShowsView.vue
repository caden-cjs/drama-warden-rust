<script setup>
import { computed, inject, onMounted, ref } from 'vue';
import { errorMessage, formatDate, mediaTypeLabel, providerTitle } from '../lib/format.js';

const api = inject('api');
const notify = inject('notify');
const refreshConnection = inject('refreshConnection');
const shows = ref([]);
const plugins = ref([]);
const loading = ref(true);
const loadError = ref('');
const query = ref('');
const mediaTypeFilter = ref('all');
const categoryFilter = ref('all');
const completionFilter = ref('all');
const sortMode = ref('name');
const showCreate = ref(false);
const showStorageModal = ref(false);
const creating = ref(false);
const searchingMetadata = ref(false);
const linkingStorage = ref(false);
const metadataResults = ref([]);
const metadataProviders = ref([]);
const selectedMetadata = ref(null);
const metadataQuery = ref('');
const detailMetadataQuery = ref('');
const detailMetadataResults = ref([]);
const detailMetadataLoading = ref(false);
const linkingMetadata = ref(null);
const detail = ref(null);
const detailLoading = ref(false);
const episodeCatalog = ref([]);
const episodeCatalogStorageId = ref('');
const episodeCatalogLoading = ref(false);
const episodeCatalogError = ref('');
const episodeSeasonFilter = ref('all');
const storageScanId = ref(null);
const resourceQuery = ref('');
const resourceResults = ref([]);
const resourceProviders = ref([]);
const resourceLoading = ref(false);
const selectedCandidate = ref(null);
const followupPreview = ref(null);
const followupLoading = ref(false);
const queueing = ref(false);
const createForm = ref({ name: '', mediaType: 'tv', category: 'tv', aliases: '' });
const storageForm = ref({ providerId: '', rootPath: '' });
let detailRequestId = 0;

const visibleShows = computed(() => {
  const needle = query.value.trim().toLowerCase();
  return shows.value.filter((show) => {
    if (mediaTypeFilter.value !== 'all' && show.mediaType !== mediaTypeFilter.value) return false;
    if (categoryFilter.value !== 'all' && (show.category || 'tv') !== categoryFilter.value) return false;
    if (completionFilter.value === 'watching' && show.finished) return false;
    if (completionFilter.value === 'finished' && !show.finished) return false;
    return !needle || show.name.toLowerCase().includes(needle)
      || (show.aliases || []).some((alias) => alias.toLowerCase().includes(needle));
  }).sort((a, b) => sortMode.value === 'name'
    ? a.name.localeCompare(b.name, 'zh-CN')
    : String(b.updatedAt || b.createdAt || '').localeCompare(String(a.updatedAt || a.createdAt || '')));
});
const watchingCount = computed(() => shows.value.filter((show) => !show.finished).length);
const finishedCount = computed(() => shows.value.filter((show) => show.finished).length);
function categoryLabel(show) {
  return ({ tv: '电视剧', anime: '动漫', variety: '综艺' })[show.category] || mediaTypeLabel(show.mediaType);
}
const storagePlugins = computed(() => plugins.value.filter((plugin) =>
  plugin.enabled && (plugin.storageContract || plugin.capabilities?.includes('listSharedFiles'))));
const discoveryStorage = computed(() => detail.value?.storages.find((storage) => storage.enabled) || null);
const catalogStorages = computed(() => detail.value?.storages || []);
const catalogSummary = computed(() => {
  const episodes = episodeCatalog.value;
  return {
    total: episodes.length,
    present: episodes.filter((episode) => episode.status === 'present').length,
    missing: episodes.filter((episode) => episode.status === 'missing').length,
    conflict: episodes.filter((episode) => episode.status === 'conflict' || episode.assetCount > 1).length,
  };
});
const catalogSeasons = computed(() => [...new Set(episodeCatalog.value.map((episode) => episode.seasonNumber))].sort((a, b) => a - b));
const visibleCatalogEpisodes = computed(() => episodeCatalog.value.filter((episode) =>
  episodeSeasonFilter.value === 'all' || episode.seasonNumber === Number(episodeSeasonFilter.value)));
const catalogSeasonGroups = computed(() => {
  const groups = new Map();
  for (const episode of visibleCatalogEpisodes.value) {
    if (!groups.has(episode.seasonNumber)) groups.set(episode.seasonNumber, []);
    groups.get(episode.seasonNumber).push(episode);
  }
  return [...groups.entries()].map(([seasonNumber, episodes]) => ({ seasonNumber, episodes }));
});

async function load() {
  loading.value = true;
  loadError.value = '';
  try {
    const [showResponse, pluginResponse] = await Promise.all([
      api.request('/api/shows'),
      api.request('/api/plugins'),
    ]);
    shows.value = showResponse.items || [];
    plugins.value = pluginResponse.items || [];
  } catch (error) {
    loadError.value = errorMessage(error);
  } finally {
    loading.value = false;
  }
}

function openCreate() {
  createForm.value = { name: '', mediaType: 'tv', category: 'tv', aliases: '' };
  metadataQuery.value = '';
  metadataResults.value = [];
  metadataProviders.value = [];
  selectedMetadata.value = null;
  showCreate.value = true;
}

async function loadEpisodeCatalog(storageId = episodeCatalogStorageId.value) {
  if (!detail.value || !storageId) {
    episodeCatalog.value = [];
    return;
  }
  episodeCatalogStorageId.value = String(storageId);
  episodeCatalogLoading.value = true;
  episodeCatalogError.value = '';
  try {
    const response = await api.request(`/api/shows/${detail.value.show.id}/storages/${storageId}/episodes`);
    episodeCatalog.value = response.items || [];
  } catch (error) {
    episodeCatalog.value = [];
    episodeCatalogError.value = errorMessage(error);
  } finally {
    episodeCatalogLoading.value = false;
  }
}

async function searchMetadata() {
  const search = metadataQuery.value.trim();
  if (!search) return;
  searchingMetadata.value = true;
  selectedMetadata.value = null;
  try {
    const params = new URLSearchParams({ q: search, mediaType: createForm.value.mediaType, limit: '12' });
    const response = await api.request('/api/metadata/search?' + params.toString());
    metadataResults.value = response.results || [];
    metadataProviders.value = response.providers || [];
    if (!metadataResults.value.length) notify('没有找到匹配的元数据候选。', 'info');
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    searchingMetadata.value = false;
  }
}

function selectMetadata(candidate) {
  selectedMetadata.value = candidate;
  if (!createForm.value.name.trim()) createForm.value.name = candidate.name || '';
}

async function createShow() {
  const name = createForm.value.name.trim();
  if (!name) return;
  creating.value = true;
  try {
    const body = {
      name,
      mediaType: createForm.value.mediaType,
      category: createForm.value.category,
      aliases: createForm.value.aliases.split(/[\n,，]/).map((item) => item.trim()).filter(Boolean),
    };
    const response = await api.request('/api/shows', { method: 'POST', body });
    let identityWarning = '';
    if (selectedMetadata.value) {
      try {
        await api.request(`/api/shows/${response.show.id}/metadata-identities`, {
          method: 'POST',
          body: {
            providerId: selectedMetadata.value.providerId,
            externalId: selectedMetadata.value.externalId,
            mediaType: selectedMetadata.value.mediaType,
            linkedBy: 'web-ui',
          },
        });
      } catch (error) {
        identityWarning = `节目已建立，但元数据关联未完成：${errorMessage(error)}`;
      }
    }
    showCreate.value = false;
    await load();
    if (identityWarning) notify(identityWarning, 'warning');
    else notify(selectedMetadata.value ? '节目已建立并关联元数据。' : '节目已添加。');
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    creating.value = false;
  }
}

async function openDetails(show) {
  const requestId = ++detailRequestId;
  detailLoading.value = true;
  resourceResults.value = [];
  followupPreview.value = null;
  selectedCandidate.value = null;
  detailMetadataResults.value = [];
  detailMetadataQuery.value = show.name;
  try {
    const [showResponse, storageResponse] = await Promise.all([
      api.request(`/api/shows/${show.id}`),
      api.request(`/api/shows/${show.id}/storages`),
    ]);
    if (requestId !== detailRequestId) return;
    detail.value = {
      show: showResponse.show,
      identities: showResponse.metadataIdentities || [],
      storages: storageResponse.items || [],
    };
    episodeCatalog.value = [];
    episodeCatalogError.value = '';
    episodeSeasonFilter.value = 'all';
    const firstStorage = (storageResponse.items || []).find((storage) => storage.enabled) || storageResponse.items?.[0];
    episodeCatalogStorageId.value = firstStorage ? String(firstStorage.id) : '';
    if (firstStorage) await loadEpisodeCatalog(firstStorage.id);
    resourceQuery.value = show.name;
    storageForm.value = { providerId: storagePlugins.value[0]?.id || '', rootPath: `/shows/${show.name.replaceAll('/', '-')}` };
  } catch (error) {
    if (requestId === detailRequestId) notify(errorMessage(error), 'error');
  } finally {
    if (requestId === detailRequestId) detailLoading.value = false;
  }
}

function closeDetails() {
  detailRequestId += 1;
  detail.value = null;
  episodeCatalog.value = [];
  episodeCatalogStorageId.value = '';
  detailLoading.value = false;
}

async function searchDetailMetadata() {
  const search = detailMetadataQuery.value.trim();
  if (!search || !detail.value) return;
  detailMetadataLoading.value = true;
  try {
    const params = new URLSearchParams({ q: search, mediaType: detail.value.show.mediaType, limit: '12' });
    const response = await api.request('/api/metadata/search?' + params.toString());
    detailMetadataResults.value = response.results || [];
    if (!detailMetadataResults.value.length) notify('没有找到匹配的元数据候选。', 'info');
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    detailMetadataLoading.value = false;
  }
}

async function linkDetailMetadata(candidate) {
  if (!detail.value) return;
  linkingMetadata.value = candidate;
  try {
    await api.request(`/api/shows/${detail.value.show.id}/metadata-identities`, {
      method: 'POST',
      body: {
        providerId: candidate.providerId,
        externalId: candidate.externalId,
        mediaType: candidate.mediaType,
        linkedBy: 'web-ui',
      },
    });
    await refreshDetail();
    notify(`已关联 ${candidate.providerId} 元数据。`);
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    linkingMetadata.value = null;
  }
}

async function refreshDetail() {
  if (detail.value) await openDetails(detail.value.show);
}

async function addStorage() {
  if (!detail.value || !storageForm.value.providerId || !storageForm.value.rootPath.trim()) return;
  linkingStorage.value = true;
  try {
    await api.request(`/api/shows/${detail.value.show.id}/storages`, {
      method: 'POST',
      body: { providerId: storageForm.value.providerId, rootPath: storageForm.value.rootPath.trim() },
    });
    showStorageModal.value = false;
    await refreshDetail();
    notify('存储位置已保存。');
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    linkingStorage.value = false;
  }
}

async function scanStorage(storage) {
  storageScanId.value = storage.id;
  try {
    const result = await api.request(`/api/shows/${detail.value.show.id}/storages/${storage.id}/scan`, { method: 'POST' });
    const count = result.assetCount ?? result.assetsSeen ?? result.assets?.length;
    notify(count == null ? '存储扫描完成。' : `扫描完成，读取到 ${count} 个媒体资产。`);
    await refreshDetail();
  } catch (error) {
    const message = error?.code === 'QUARK_AUTH_FAILED' || error?.code === 'PAN115_AUTH_FAILED'
      ? `${storage.providerId === 'pan115' ? '115' : '夸克'}授权已失效，请完成官方客户端登录后重新扫描。`
      : error?.code === 'QUARK_LIST_FAILED'
        ? '夸克目录读取失败，请在插件管理页检查授权状态和 CLI 配置后重新扫描。'
      : error?.code === 'STORAGE_PATH_NOT_FOUND'
        ? `${storage.providerId === 'pan115' ? '115' : '网盘'}目录不存在：${storage.rootPath}。请改为网盘中实际存在的目录路径后重新扫描。`
        : error?.code === 'PAN115_CLI_UNAVAILABLE'
          ? '115 CLI 不可用，请检查本机 115 CLI 安装和配置路径。'
          : errorMessage(error);
    episodeCatalogError.value = message;
    notify(message, 'error');
  } finally {
    storageScanId.value = null;
  }
}

async function searchResources(storageId) {
  resourceLoading.value = true;
  selectedCandidate.value = null;
  followupPreview.value = null;
  try {
    const response = await api.request(`/api/shows/${detail.value.show.id}/resources/search`, {
      method: 'POST',
      body: { storageId, query: resourceQuery.value.trim() || detail.value.show.name, limit: 10 },
    });
    resourceResults.value = response.results || [];
    resourceProviders.value = response.providers || [];
    if (!resourceResults.value.length) notify('没有找到资源候选。', 'info');
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    resourceLoading.value = false;
  }
}

async function previewCandidate(candidate, storageId) {
  selectedCandidate.value = candidate;
  followupPreview.value = null;
  followupLoading.value = true;
  try {
    const response = await api.request(`/api/shows/${detail.value.show.id}/followup/preview`, {
      method: 'POST',
      body: { storageId, shareUrl: candidate.shareUrl, shareTitle: candidate.title || '' },
    });
    followupPreview.value = response.preview;
  } catch (error) {
    notify(previewFailureLabel(error?.code) || errorMessage(error), 'error');
  } finally {
    followupLoading.value = false;
  }
}

function previewFailureLabel(code) {
  return ({
    QUARK_SHARE_UNAVAILABLE: '这个夸克分享链接已失效或不可访问，请换一个候选。',
    QUARK_SHARE_PASSCODE_INVALID: '这个夸克分享的提取码缺失或不正确。',
    QUARK_SHARE_RATE_LIMITED: '夸克暂时限制了分享查询，请稍后重试。',
    STORAGE_PATH_NOT_FOUND: '网盘目录不存在，请检查当前存储位置配置。',
  })[code] || null;
}

function candidatePreviewLabel(candidate) {
  const preview = candidate?.preview;
  if (preview?.eligible) return '预览可追更';
  return previewFailureLabel(preview?.errorCode) || preview?.reason || '需要复核';
}

async function queueFollowup() {
  if (!detail.value || !selectedCandidate.value || !followupPreview.value?.eligible) return;
  const storageId = followupPreview.value.storage?.id;
  if (!storageId) return;
  queueing.value = true;
  try {
    const response = await api.request(`/api/shows/${detail.value.show.id}/followup/queue`, {
      method: 'POST',
      body: {
        storageId,
        shareUrl: selectedCandidate.value.shareUrl,
        shareTitle: selectedCandidate.value.title || '',
      },
    });
    if (response.queued) {
      notify('候选已加入追更事件队列；媒体尚未转存。');
      closeDetails();
      await refreshConnection();
    } else {
      notify(response.preview?.reason || '该候选目前无法加入队列。', 'warning');
    }
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    queueing.value = false;
  }
}

function startAddStorage() {
  if (!storagePlugins.value.length) {
    notify('没有已启用的存储插件，请先配置并启用存储 provider。', 'warning');
    return;
  }
  storageForm.value.providerId = storagePlugins.value[0].id;
  showStorageModal.value = true;
}

onMounted(load);
</script>

<template>
  <div class="view-stack">
    <div class="page-intro">
      <div><h2>管理你的追更清单。</h2><p>维护节目、元数据来源和媒体库位置。</p></div>
      <n-button class="button button-primary" attr-type="button" @click="openCreate"><span>＋</span> 添加节目</n-button>
    </div>

    <div class="show-library-summary">
      <div><span>节目总数</span><strong>{{ shows.length }}</strong></div>
      <div><span>追更中</span><strong>{{ watchingCount }}</strong></div>
      <div><span>已完结</span><strong>{{ finishedCount }}</strong></div>
    </div>

    <section class="panel toolbar-panel">
      <div class="search-field"><span aria-hidden="true">⌕</span><n-input v-model:value="query" clearable type="search" placeholder="搜索节目或别名" /></div>
      <div class="show-list-controls">
        <n-select v-model:value="mediaTypeFilter" class="form-control" aria-label="媒体类型" :options="[{ label: '全部媒体类型', value: 'all' }, { label: '剧集', value: 'tv' }, { label: '电影', value: 'movie' }]" />
        <n-select v-model:value="categoryFilter" class="form-control" aria-label="节目分类" :options="[{ label: '全部分类', value: 'all' }, { label: '电视剧', value: 'tv' }, { label: '动漫', value: 'anime' }, { label: '综艺', value: 'variety' }]" />
        <n-select v-model:value="completionFilter" class="form-control" aria-label="追更状态" :options="[{ label: '全部状态', value: 'all' }, { label: '追更中', value: 'watching' }, { label: '已完结', value: 'finished' }]" />
        <n-select v-model:value="sortMode" class="form-control" aria-label="排序方式" :options="[{ label: '按名称', value: 'name' }, { label: '最近更新', value: 'updated' }]" />
        <span class="subtle-count">显示 {{ visibleShows.length }} / {{ shows.length }}</span><n-button class="icon-button small" attr-type="button" title="刷新节目列表" @click="load">⟳</n-button>
      </div>
    </section>

    <div v-if="loadError" class="inline-alert error-alert"><span class="alert-symbol">!</span><div><strong>节目列表加载失败</strong><p>{{ loadError }}</p></div></div>
    <div v-if="loading" class="loading-grid"><div v-for="index in 4" :key="index" class="skeleton-card"></div></div>
    <div v-else-if="!visibleShows.length" class="panel empty-state shows-empty">
      <div class="empty-illustration">▣</div><strong>{{ query ? '没有匹配的节目' : '节目库还是空的' }}</strong>
      <p>{{ query ? '换一个名称或别名试试。' : '添加节目后，关联元数据并配置一个存储位置。' }}</p>
      <n-button v-if="!query" class="button button-primary" attr-type="button" @click="openCreate">添加第一部节目</n-button>
    </div>
    <div v-else class="show-grid">
      <button v-for="show in visibleShows" :key="show.id" class="show-card" type="button" @click="openDetails(show)">
        <div class="show-card-top"><div class="poster-placeholder"><span>{{ (show.name || '?').slice(0, 1) }}</span></div><span class="show-type">{{ categoryLabel(show) }}</span></div>
        <div class="show-card-copy"><h3>{{ show.name }}</h3><div v-if="show.aliases?.length" class="show-alias">{{ show.aliases.slice(0, 2).join(' · ') }}</div><div class="show-card-footer"><span class="status-dot" :class="show.finished ? 'finished' : 'watching'"></span>{{ show.finished ? '已完结' : '追更中' }}<span class="show-id">#{{ show.id }}</span></div></div>
        <span class="show-arrow" aria-hidden="true">↗</span>
      </button>
    </div>

    <Teleport to="body">
      <div v-if="showCreate" class="modal-backdrop" @click.self="showCreate = false">
        <section class="modal modal-wide" role="dialog" aria-modal="true" aria-labelledby="create-show-title">
          <div class="modal-heading"><div><span class="modal-kicker">节目库 / 新建</span><h2 id="create-show-title">添加节目</h2><p>选择元数据候选可以自动带入节目名称并建立来源关联。</p></div><n-button class="icon-button" attr-type="button" aria-label="关闭" @click="showCreate = false">×</n-button></div>
          <form class="form-stack" @submit.prevent="createShow">
            <div class="form-row two-columns"><label class="field"><span>节目名称</span><n-input v-model:value="createForm.name" placeholder="例如：星际牛仔" :input-props="{ required: true, maxlength: 255 }" /></label><label class="field"><span>媒体类型</span><n-select v-model:value="createForm.mediaType" :options="[{ label: '剧集', value: 'tv' }, { label: '电影', value: 'movie' }]" /></label></div>
            <label v-if="createForm.mediaType === 'tv'" class="field"><span>节目分类</span><n-select v-model:value="createForm.category" :options="[{ label: '电视剧', value: 'tv' }, { label: '动漫', value: 'anime' }, { label: '综艺', value: 'variety' }]" /></label>
            <label class="field"><span>别名 <small>选填，用逗号分隔</small></span><n-input v-model:value="createForm.aliases" placeholder="英文名、其他译名" /></label>
            <div class="section-rule"><span>元数据关联</span></div>
            <div class="inline-search"><n-input v-model:value="metadataQuery" placeholder="搜索 TMDB / TVmaze" @keydown.enter.prevent="searchMetadata" /><n-button class="button button-secondary" attr-type="button" :disabled="searchingMetadata || !metadataQuery.trim()" @click="searchMetadata">{{ searchingMetadata ? '搜索中…' : '搜索' }}</n-button></div>
            <div v-if="metadataProviders.length" class="provider-summary">搜索来源：{{ metadataProviders.map((item) => item.id || item.providerId || item).join(' · ') }}</div>
            <div v-if="metadataResults.length" class="metadata-results">
              <button v-for="candidate in metadataResults" :key="`${candidate.providerId}-${candidate.externalId}`" class="metadata-result" :class="{ selected: selectedMetadata === candidate }" type="button" @click="selectMetadata(candidate)">
                <span class="metadata-radio">{{ selectedMetadata === candidate ? '✓' : '' }}</span>
                <span class="metadata-result-main"><strong>{{ candidate.name }}</strong><small>{{ candidate.originalName && candidate.originalName !== candidate.name ? candidate.originalName + ' · ' : '' }}{{ candidate.year || candidate.firstAirDate || '年份未知' }}</small></span>
                <span class="provider-tag">{{ candidate.providerId }}</span>
              </button>
            </div>
            <div v-else class="helper-panel">未选择候选时仍可先添加节目，之后再通过其他 API 或管理工具关联元数据。</div>
            <div class="modal-actions"><n-button class="button button-quiet" attr-type="button" @click="showCreate = false">取消</n-button><n-button class="button button-primary" attr-type="submit" :disabled="creating || !createForm.name.trim()">{{ creating ? '正在保存…' : selectedMetadata ? '创建并关联' : '创建节目' }}</n-button></div>
          </form>
        </section>
      </div>

      <div v-if="detail || detailLoading" class="modal-backdrop detail-backdrop" @click.self="closeDetails">
        <section v-if="detail" class="detail-drawer" role="dialog" aria-modal="true" :aria-labelledby="`show-title-${detail.show.id}`">
          <div class="detail-heading"><n-button class="icon-button" attr-type="button" aria-label="关闭" @click="closeDetails">←</n-button><div class="detail-heading-copy"><span class="modal-kicker">节目详情 / #{{ detail.show.id }}</span><h2 :id="`show-title-${detail.show.id}`">{{ detail.show.name }}</h2></div><n-button class="icon-button" attr-type="button" aria-label="刷新详情" @click="refreshDetail">⟳</n-button></div>
          <div class="detail-scroll">
            <div class="detail-summary-card"><div class="poster-placeholder poster-large"><span>{{ detail.show.name.slice(0, 1) }}</span></div><div><span class="status-dot" :class="detail.show.finished ? 'finished' : 'watching'"></span><strong>{{ detail.show.finished ? '已完结' : '追更中' }}</strong><p>{{ categoryLabel(detail.show) }} · 添加于 {{ formatDate(detail.show.createdAt) }}</p><small v-if="detail.show.aliases?.length">别名：{{ detail.show.aliases.join('、') }}</small></div></div>
            <section class="detail-section"><div class="section-heading"><div><h3>元数据来源</h3><p>用于确认季集清单和缺集计划</p></div><span class="subtle-count">{{ detail.identities.length }} 个</span></div><div v-if="!detail.identities.length" class="detail-empty">尚未关联元数据。搜索节目候选并明确选择对应的来源。</div><div v-else class="identity-list"><div v-for="identity in detail.identities" :key="identity.id" class="identity-row"><span class="identity-logo">{{ identity.providerId.slice(0, 1).toUpperCase() }}</span><div><strong>{{ identity.providerId }}</strong><small>{{ identity.mediaType }} · 外部 ID {{ identity.externalId }}</small></div><span class="identity-date">{{ formatDate(identity.linkedAt) }}</span></div></div><div class="inline-search detail-metadata-search"><n-input v-model:value="detailMetadataQuery" placeholder="搜索其他元数据来源" @keydown.enter.prevent="searchDetailMetadata" /><n-button class="button button-secondary" attr-type="button" :disabled="detailMetadataLoading || !detailMetadataQuery.trim()" @click="searchDetailMetadata">{{ detailMetadataLoading ? '搜索中…' : '搜索' }}</n-button></div><div v-if="detailMetadataResults.length" class="metadata-results detail-candidates"><div v-for="candidate in detailMetadataResults" :key="`${candidate.providerId}-${candidate.externalId}`" class="metadata-result"><span class="metadata-result-main"><strong>{{ candidate.name }}</strong><small>{{ candidate.providerId }} · {{ candidate.year || candidate.firstAirDate || '年份未知' }}</small></span><n-button class="button button-quiet button-small" attr-type="button" :disabled="linkingMetadata === candidate" @click="linkDetailMetadata(candidate)">{{ linkingMetadata === candidate ? '关联中…' : '关联' }}</n-button></div></div></section>
            <section class="detail-section"><div class="section-heading"><div><h3>媒体库位置</h3><p>扫描会读取目录并同步集资产</p></div><n-button class="button button-secondary button-small" attr-type="button" @click="startAddStorage">＋ 添加位置</n-button></div><div v-if="!detail.storages.length" class="detail-empty">还没有配置存储位置。启用本地存储插件后，可以添加一个媒体目录。</div><div v-else class="storage-list"><div v-for="storage in detail.storages" :key="storage.id" class="storage-row"><div class="storage-icon">▤</div><div class="storage-copy"><strong>{{ storage.rootPath }}</strong><small>{{ storage.providerId }} · {{ storage.enabled ? '已启用' : '已停用' }}</small></div><n-button class="button button-quiet button-small" attr-type="button" :disabled="storageScanId === storage.id || !storage.enabled" @click="scanStorage(storage)">{{ storageScanId === storage.id ? '扫描中…' : '扫描目录' }}</n-button></div></div></section>
            <section class="detail-section episode-catalog-section">
              <div class="section-heading catalog-heading">
                <div><h3>逐集入库目录</h3><p>最近一次存储扫描的数据库快照，不会在打开详情时重新扫描</p></div>
                <div class="catalog-controls">
                  <n-select v-if="catalogStorages.length > 1" v-model:value="episodeCatalogStorageId" class="form-control" aria-label="选择存储位置" :options="catalogStorages.map((storage) => ({ label: `${providerTitle(plugins.find((plugin) => plugin.id === storage.providerId) || { id: storage.providerId })} · ${storage.rootPath}`, value: String(storage.id) }))" @update:value="loadEpisodeCatalog()" />
                  <n-select v-if="catalogSeasons.length > 1" v-model:value="episodeSeasonFilter" class="form-control" aria-label="按季筛选" :options="[{ label: '全部季', value: 'all' }, ...catalogSeasons.map((season) => ({ label: `第 ${season} 季`, value: String(season) }))]" />
                  <n-button class="button button-quiet button-small" attr-type="button" :disabled="episodeCatalogLoading || !episodeCatalogStorageId" @click="loadEpisodeCatalog()">{{ episodeCatalogLoading ? '读取中…' : '刷新目录' }}</n-button>
                </div>
              </div>
              <div v-if="episodeCatalogError" class="inline-alert error-alert"><span class="alert-symbol">!</span><div><strong>目录读取失败</strong><p>{{ episodeCatalogError }}</p></div></div>
              <div v-else-if="!catalogStorages.length" class="detail-empty">先添加存储位置并扫描目录，逐集进度才会显示在这里。</div>
              <div v-else-if="episodeCatalogLoading && !episodeCatalog.length" class="catalog-loading"><div class="spinner"></div><span>正在读取已保存的目录快照</span></div>
              <div v-else-if="!episodeCatalog.length" class="detail-empty">暂无逐集快照。点击上方存储位置中的“扫描目录”后再刷新。</div>
              <template v-else>
                <div class="catalog-summary">
                  <div><strong>{{ catalogSummary.present }}</strong><span>已入库</span></div>
                  <div><strong>{{ catalogSummary.missing }}</strong><span>缺失</span></div>
                  <div><strong>{{ catalogSummary.conflict }}</strong><span>多版本待核对</span></div>
                  <div><strong>{{ catalogSummary.total }}</strong><span>目录集数</span></div>
                </div>
                <div class="catalog-progress"><span :style="{ width: `${catalogSummary.total ? Math.round(catalogSummary.present / catalogSummary.total * 100) : 0}%` }"></span></div>
                <div v-for="group in catalogSeasonGroups" :key="group.seasonNumber" class="catalog-season-group">
                  <h4 v-if="catalogSeasons.length > 1 && episodeSeasonFilter === 'all'">第 {{ group.seasonNumber }} 季</h4>
                  <div class="episode-grid">
                    <div v-for="episode in group.episodes" :key="`${episode.seasonNumber}-${episode.episodeNumber}`" class="episode-cell" :class="`episode-${episode.status}`" :title="`${episode.assetCount} 个文件${episode.scannedAt ? ` · 扫描于 ${formatDate(episode.scannedAt)}` : ''}`">
                      <strong>E{{ String(episode.episodeNumber).padStart(2, '0') }}</strong>
                      <small>{{ episode.status === 'present' ? '已入库' : episode.status === 'conflict' || episode.assetCount > 1 ? `${episode.assetCount} 个版本` : '缺失' }}</small>
                    </div>
                  </div>
                </div>
              </template>
            </section>
            <section v-if="detail.show.mediaType === 'tv'" class="detail-section followup-section"><div class="section-heading"><div><h3>资源候选</h3><p>搜索并预览，不会直接转存媒体</p></div></div><div v-if="!detail.storages.length" class="detail-empty">先添加存储位置，再搜索资源候选。</div><div v-else-if="!discoveryStorage" class="detail-empty">当前存储位置都已停用，请先在服务端启用一个位置。</div><template v-else><div class="inline-search resource-search"><n-input v-model:value="resourceQuery" placeholder="资源搜索关键词" @keydown.enter.prevent="searchResources(discoveryStorage.id)" /><n-button class="button button-secondary" attr-type="button" :disabled="resourceLoading" @click="searchResources(discoveryStorage.id)">{{ resourceLoading ? '搜索中…' : '搜索资源' }}</n-button></div><div v-if="resourceProviders.length" class="provider-summary">{{ resourceProviders.map((item) => `${item.providerId}${item.ok ? '' : '（失败）'}`).join(' · ') }}</div><div v-if="resourceResults.length" class="resource-results"><article v-for="(candidate, index) in resourceResults" :key="candidate.sourceId || index" class="resource-card" :class="{ chosen: selectedCandidate === candidate }"><div><strong>{{ candidate.title || '未命名资源' }}</strong><small>{{ candidate.providerId }} · {{ candidatePreviewLabel(candidate) }}</small></div><n-button class="button button-quiet button-small" attr-type="button" :disabled="followupLoading" @click="previewCandidate(candidate, discoveryStorage.id)">{{ selectedCandidate === candidate && followupLoading ? '预览中…' : '预览' }}</n-button></article></div><div v-if="followupPreview" class="preview-card" :class="followupPreview.eligible ? 'preview-good' : 'preview-muted'"><div class="preview-head"><strong>{{ followupPreview.eligible ? '确认到可追集内容' : '当前无法追更' }}</strong><span>{{ followupPreview.reason || '计划已生成' }}</span></div><p v-if="followupPreview.plan">S{{ String(followupPreview.plan.seasonNumber).padStart(2, '0') }} · 本次选择 {{ followupPreview.plan.saveBatch?.length || 0 }} 集<span v-if="followupPreview.plan.deferredCount">，另有 {{ followupPreview.plan.deferredCount }} 集留待后续</span></p><div v-if="followupPreview.plan?.saveBatch?.length" class="episode-chips"><span v-for="file in followupPreview.plan.saveBatch" :key="file.fileId || file.id || file.episodeNumber">E{{ String(file.episodeNumber || file.episode || '?').padStart(2, '0') }}</span></div><n-button v-if="followupPreview.eligible" class="button button-primary button-full" attr-type="button" :disabled="queueing" @click="queueFollowup">{{ queueing ? '正在加入…' : '加入追更队列' }}</n-button><small v-if="followupPreview.eligible" class="helper-note">加入队列只创建待处理事件，不会写入媒体文件。</small></div></template></section><section v-else class="detail-section"><div class="section-heading"><div><h3>资源候选</h3><p>当前资源追踪流程针对剧集设计</p></div></div><div class="detail-empty">电影的资源追踪和存储流程尚未实现。</div></section>
          </div>
        </section>
        <section v-else class="modal loading-modal"><div class="spinner"></div><strong>正在加载节目详情</strong></section>
      </div>

      <div v-if="showStorageModal" class="modal-backdrop nested-backdrop" @click.self="showStorageModal = false">
        <section class="modal" role="dialog" aria-modal="true" aria-labelledby="storage-title"><div class="modal-heading"><div><span class="modal-kicker">{{ detail?.show.name }} / 存储</span><h2 id="storage-title">添加媒体库位置</h2><p>位置使用对应存储插件的路径语义。</p></div><n-button class="icon-button" attr-type="button" aria-label="关闭" @click="showStorageModal = false">×</n-button></div><form class="form-stack" @submit.prevent="addStorage"><label class="field"><span>存储插件</span><n-select v-model:value="storageForm.providerId" :options="storagePlugins.map((plugin) => ({ label: providerTitle(plugin), value: plugin.id }))" /></label><label class="field"><span>目录路径</span><n-input v-model:value="storageForm.rootPath" placeholder="/shows/节目名" :input-props="{ required: true, maxlength: 600 }" /><small v-if="storageForm.providerId === 'local-storage'">本地存储要求以 / 开头，并且不能是根目录。</small></label><div class="modal-actions"><n-button class="button button-quiet" attr-type="button" @click="showStorageModal = false">取消</n-button><n-button class="button button-primary" attr-type="submit" :disabled="linkingStorage">{{ linkingStorage ? '保存中…' : '保存位置' }}</n-button></div></form></section>
      </div>
    </Teleport>
  </div>
</template>
