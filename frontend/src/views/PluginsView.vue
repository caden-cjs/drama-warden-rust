<script setup>
import { computed, inject, onMounted, ref } from 'vue';
import { errorMessage, formatDate, providerTitle } from '../lib/format.js';

const api = inject('api');
const notify = inject('notify');
const plugins = ref([]);
const loading = ref(true);
const busyPlugins = ref({});
const pluginChecks = ref({});
const pluginCalls = ref([]);
const callsLoading = ref(false);
const callOffset = ref(0);
const callsHasMore = ref(false);
const callPluginFilter = ref('');
const callOutcomeFilter = ref('all');
const configPlugin = ref(null);
const pluginConfig = ref(null);
const configDraft = ref({});
const configBusy = ref(false);
const groups = computed(() => {
  const entries = [
    ['metadata', '元数据'],
    ['resource', '资源搜索'],
    ['storage', '存储'],
    ['indexer', '索引'],
    ['notification', '通知'],
    ['subtitle', '字幕'],
    ['workflow', '工作流'],
  ];
  return entries.map(([domain, label]) => ({
    domain,
    label,
    items: plugins.value.filter((plugin) => Object.values(plugin.capabilityDomains || {})
      .some((domains) => domains.includes(domain))),
  })).filter((group) => group.items.length);
});

async function load() {
  loading.value = true;
  try {
    const response = await api.request('/api/plugins');
    plugins.value = response.items || [];
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    loading.value = false;
  }
  await loadCalls();
}

async function loadCalls({ append = false } = {}) {
  callsLoading.value = true;
  const offset = append ? callOffset.value : 0;
  const query = new URLSearchParams({ limit: '100', offset: String(offset) });
  if (callPluginFilter.value) query.set('pluginId', callPluginFilter.value);
  if (callOutcomeFilter.value !== 'all') {
    query.set('ok', callOutcomeFilter.value === 'success' ? 'true' : 'false');
  }
  try {
    const response = await api.request(`/api/plugin-calls?${query}`);
    const items = response.items || [];
    pluginCalls.value = append ? [...pluginCalls.value, ...items] : items;
    callOffset.value = offset + items.length;
    callsHasMore.value = items.length === 100;
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    callsLoading.value = false;
  }
}

async function changeCallFilter() {
  await loadCalls();
}

function pluginCallErrorLabel(code) {
  const labels = {
    PLUGIN_ERROR: '旧记录未保存原始错误；请运行自检后重试以获取更具体的安全错误码',
    PLUGIN_TIMEOUT: '插件调用超时，请检查服务响应与超时配置',
    INVALID_STORAGE_PATH: '存储路径格式无效，请检查路径配置',
    STORAGE_PATH_NOT_FOUND: '存储目录不存在或当前账号无法访问',
    QUARK_CLI_UNAVAILABLE: '夸克 CLI 未配置或无法启动',
    PAN115_CLI_UNAVAILABLE: '115 CLI 未配置或无法启动',
    QUARK_CLI_PERMISSION_DENIED: '服务进程没有执行夸克 CLI 的权限',
    PAN115_CLI_PERMISSION_DENIED: '服务进程没有执行 115 CLI 的权限',
    QUARK_AUTH_FAILED: '夸克授权失效，请检查 CLI 登录状态',
    QUARK_SHARE_UNAVAILABLE: '夸克分享链接已失效或不可访问，请换一个候选。',
    QUARK_SHARE_PASSCODE_INVALID: '夸克分享提取码缺失或不正确。',
    QUARK_SHARE_RATE_LIMITED: '夸克暂时限制了分享查询，请稍后重试。',
    QUARK_LISTSHAREDFILES_FAILED: '夸克分享清单读取失败；旧记录未保留具体原因，请重新预览该候选。',
    PAN115_AUTH_FAILED: '115 授权失败，请检查 Cookie 或登录状态',
    QUARK_LIST_FAILED: '夸克目录读取失败，请检查 CLI 授权和目标目录',
    PAN115_LIST_FAILED: '115 目录读取失败，请检查 CLI 授权和目标目录',
  };
  return labels[code] || '插件调用失败；可运行插件自检并查看服务端日志';
}

function canMove(plugin, direction) {
  const peers = plugins.value.filter((item) => item.kind === plugin.kind)
    .sort((left, right) => left.order - right.order || left.id.localeCompare(right.id));
  const index = peers.findIndex((item) => item.id === plugin.id);
  return direction === 'up' ? index > 0 : index >= 0 && index < peers.length - 1;
}

async function updatePlugin(plugin, body, message) {
  busyPlugins.value = { ...busyPlugins.value, [plugin.id]: true };
  try {
    const response = await api.request(`/api/plugins/${encodeURIComponent(plugin.id)}`, {
      method: 'PUT',
      body,
    });
    plugins.value = response.items || [];
    notify(message);
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    busyPlugins.value = { ...busyPlugins.value, [plugin.id]: false };
  }
}

async function togglePlugin(plugin) {
  await updatePlugin(
    plugin,
    { enabled: !plugin.enabled },
    plugin.enabled ? '插件已停用。' : '插件已启用。',
  );
}

async function movePlugin(plugin, direction) {
  busyPlugins.value = { ...busyPlugins.value, [plugin.id]: true };
  try {
    const response = await api.request(`/api/plugins/${encodeURIComponent(plugin.id)}/move`, {
      method: 'POST',
      body: { direction },
    });
    plugins.value = response.items || [];
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    busyPlugins.value = { ...busyPlugins.value, [plugin.id]: false };
  }
}

async function checkPlugin(plugin) {
  busyPlugins.value = { ...busyPlugins.value, [plugin.id]: true };
  try {
    const response = await api.request(`/api/plugins/${encodeURIComponent(plugin.id)}/monitor`, {
      method: 'POST',
    });
    const result = response.result;
    pluginChecks.value = { ...pluginChecks.value, [plugin.id]: result };
    notify(result.ok ? '插件自检通过。' : `插件自检未通过${result.errorCode ? `：${result.errorCode}` : '。'}`,
      result.ok ? 'success' : 'warning');
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    busyPlugins.value = { ...busyPlugins.value, [plugin.id]: false };
  }
}

async function resetPlugin(plugin) {
  if (!window.confirm('将执行此插件实现的 reset 操作。具体效果取决于插件代码，请只对可信插件使用。继续吗？')) return;
  busyPlugins.value = { ...busyPlugins.value, [plugin.id]: true };
  try {
    await api.request(`/api/plugins/${encodeURIComponent(plugin.id)}/reset`, { method: 'POST' });
    pluginChecks.value = { ...pluginChecks.value, [plugin.id]: null };
    notify('插件重置完成。', 'info');
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    busyPlugins.value = { ...busyPlugins.value, [plugin.id]: false };
  }
}

async function openConfig(plugin) {
  configBusy.value = true;
  configPlugin.value = plugin;
  try {
    const response = await api.request(`/api/plugins/${encodeURIComponent(plugin.id)}/config`);
    pluginConfig.value = response;
    configDraft.value = Object.fromEntries((response.fields || []).map((field) => [field.key, field.secret ? '' : (field.value ?? '')]));
  } catch (error) {
    notify(errorMessage(error), 'error');
    configPlugin.value = null;
  } finally {
    configBusy.value = false;
  }
}

async function saveConfig() {
  if (!configPlugin.value || !pluginConfig.value) return;
  configBusy.value = true;
  try {
    const config = {};
    for (const field of pluginConfig.value.fields || []) {
      if (!field.editable) continue;
      const value = configDraft.value[field.key];
      if (value === '') continue;
      config[field.key] = field.type === 'integer' && value !== null ? Number(value) : value;
    }
    await api.request(`/api/plugins/${encodeURIComponent(configPlugin.value.id)}/config`, {
      method: 'PATCH', body: { config, expectedRevision: pluginConfig.value.revision },
    });
    notify('插件配置已保存。');
    configPlugin.value = null;
    await load();
  } catch (error) {
    notify(errorMessage(error), 'error');
  } finally {
    configBusy.value = false;
  }
}

onMounted(load);
</script>

<template>
  <div class="view-stack">
    <div class="page-intro"><div><h2>能力由插件提供，边界清晰。</h2><p>管理启停和调用顺序，并检查各插件的运行状态。</p></div><button class="button button-secondary" type="button" :disabled="loading" @click="load"><span class="button-icon">⟳</span>{{ loading ? '正在刷新' : '刷新插件' }}</button></div>
    <div class="plugin-summary-grid"><div class="plugin-summary-card"><span class="metric-icon lilac">◇</span><div><strong>{{ plugins.length }}</strong><small>已注册插件</small></div></div><div class="plugin-summary-card"><span class="metric-icon mint">✓</span><div><strong>{{ plugins.filter((plugin) => plugin.enabled).length }}</strong><small>正在启用</small></div></div><div class="plugin-summary-card"><span class="metric-icon blue">⌘</span><div><strong>{{ new Set(plugins.flatMap((plugin) => plugin.capabilities || [])).size }}</strong><small>声明能力</small></div></div></div>
    <div v-if="loading" class="loading-grid"><div v-for="index in 3" :key="index" class="skeleton-card"></div></div>
    <div v-else-if="!plugins.length" class="panel empty-state"><div class="empty-illustration">◇</div><strong>当前没有注册插件</strong><p>检查服务启动配置和插件加载日志。</p></div>
    <section v-for="group in groups" :key="group.domain" class="plugin-group">
      <div class="plugin-group-heading"><div><span class="eyebrow">CAPABILITY DOMAIN</span><h3>{{ group.label }}</h3></div><span class="subtle-count">{{ group.items.length }} 个插件</span></div>
      <div class="plugin-grid"><article v-for="plugin in group.items" :key="`${group.domain}-${plugin.id}`" class="panel plugin-card"><div class="plugin-card-head"><div class="plugin-mark" :class="plugin.active ? 'plugin-on' : ''">{{ (plugin.label || plugin.name || plugin.id).slice(0, 1).toUpperCase() }}</div><span class="status-chip" :class="plugin.active ? 'chip-processed' : 'chip-muted'">{{ plugin.active ? '运行中' : plugin.configured ? (plugin.enabled ? '待启动' : '已停用') : '待配置' }}</span></div><h4>{{ providerTitle(plugin) }}</h4><div class="plugin-id">{{ plugin.id }}</div><div class="plugin-meta"><span>{{ plugin.kind }}</span><span>v{{ plugin.version }}</span><span>API v{{ plugin.apiVersion }}</span></div><div class="capability-list"><span v-for="capability in plugin.capabilities" :key="capability">{{ capability }}</span></div><div class="plugin-controls"><div class="plugin-order-controls"><small>同类顺序 {{ plugin.order + 1 }}</small><button class="button button-quiet button-small" type="button" title="上移" :aria-label="`${providerTitle(plugin)} 上移`" :disabled="busyPlugins[plugin.id] || !canMove(plugin, 'up')" @click="movePlugin(plugin, 'up')">↑</button><button class="button button-quiet button-small" type="button" title="下移" :aria-label="`${providerTitle(plugin)} 下移`" :disabled="busyPlugins[plugin.id] || !canMove(plugin, 'down')" @click="movePlugin(plugin, 'down')">↓</button></div><div class="plugin-action-controls"><button class="button button-quiet button-small" type="button" @click="openConfig(plugin)">配置</button><button class="button button-quiet button-small" type="button" :disabled="busyPlugins[plugin.id]" @click="checkPlugin(plugin)">{{ busyPlugins[plugin.id] ? '处理中…' : '自检' }}</button><button class="button button-quiet button-small" type="button" :disabled="busyPlugins[plugin.id]" @click="resetPlugin(plugin)">重置插件</button><button class="button button-small" :class="plugin.enabled ? 'button-quiet' : 'button-primary'" type="button" :disabled="busyPlugins[plugin.id] || (!plugin.configured && !plugin.enabled)" @click="togglePlugin(plugin)">{{ plugin.enabled ? '停用' : '启用' }}</button></div></div><div v-if="pluginChecks[plugin.id]" class="plugin-check-result" :class="pluginChecks[plugin.id].ok ? 'check-ok' : 'check-error'">{{ pluginChecks[plugin.id].ok ? '最近自检通过' : `最近自检未通过${pluginChecks[plugin.id].errorCode ? ` · ${pluginChecks[plugin.id].errorCode}` : ''}` }}<span v-if="pluginChecks[plugin.id].configured === false"> · 未配置</span><span v-if="pluginChecks[plugin.id].authorized === false"> · 未授权</span></div></article></div>
    </section>
    <div v-if="configPlugin" class="dialog-backdrop" @click.self="configPlugin = null"><section class="panel plugin-config-dialog" role="dialog" aria-modal="true" :aria-label="`${providerTitle(configPlugin)} 配置`"><div class="section-heading"><div><span class="eyebrow">PLUGIN CONFIGURATION</span><h3>{{ providerTitle(configPlugin) }} 配置</h3></div><button class="button button-quiet button-small" type="button" @click="configPlugin = null">关闭</button></div><div v-if="configBusy && !pluginConfig" class="loading-state">正在读取配置…</div><template v-else-if="pluginConfig"><label v-for="field in pluginConfig.fields" :key="field.key" class="form-field"><span>{{ field.label || field.key }}<small v-if="field.required"> · 必填</small><small v-if="field.scope === 'deployment'"> · 部署配置</small></span><small v-if="field.description">{{ field.description }}</small><small>来源：{{ { database: '插件数据库', legacyEnvironment: '现有环境配置', deployment: '部署配置', manifestDefault: '插件默认值', unset: '未配置' }[field.source] || field.source }}</small><input v-if="field.type !== 'boolean' && field.type !== 'select'" v-model="configDraft[field.key]" class="form-control" :type="field.secret ? 'password' : field.type === 'url' ? 'url' : field.type === 'integer' ? 'number' : 'text'" :placeholder="field.secret ? (field.configured ? '已配置，留空保持不变' : '输入凭据') : (field.placeholder || '')" :disabled="!field.editable"><select v-else-if="field.type === 'select'" v-model="configDraft[field.key]" class="form-control" :disabled="!field.editable"><option v-for="option in field.options || []" :key="option.value" :value="option.value">{{ option.label || option.value }}</option></select><input v-else v-model="configDraft[field.key]" type="checkbox" :disabled="!field.editable"><button v-if="field.configured && !field.required && field.editable" class="button button-quiet button-small" type="button" @click="configDraft[field.key] = null">{{ field.secret ? '清除已保存凭据' : '恢复环境或默认值' }}</button></label><div class="dialog-actions"><button class="button button-secondary" type="button" @click="configPlugin = null">取消</button><button class="button button-primary" type="button" :disabled="configBusy" @click="saveConfig">{{ configBusy ? '保存中…' : '保存配置' }}</button></div></template></section></div>
    <section class="panel event-table-panel plugin-call-panel">
      <div class="plugin-call-heading"><div><span class="eyebrow">CAPABILITY AUDIT</span><h3>插件调用记录</h3><p>记录调用能力、来源、关联 ID、结果与耗时；不存储请求或响应内容。</p></div><div class="plugin-call-filters"><select v-model="callPluginFilter" class="form-control" aria-label="按插件筛选" @change="changeCallFilter"><option value="">全部插件</option><option v-for="plugin in plugins" :key="plugin.id" :value="plugin.id">{{ providerTitle(plugin) }}</option></select><select v-model="callOutcomeFilter" class="form-control" aria-label="按结果筛选" @change="changeCallFilter"><option value="all">全部结果</option><option value="success">成功</option><option value="failed">失败</option></select></div></div>
      <div class="table-heading plugin-call-table-heading"><span>插件 / 能力</span><span>结果</span><span>来源</span><span>耗时</span><span>调用时间</span><span>关联 ID</span></div>
      <div v-if="callsLoading && !pluginCalls.length" class="table-loading"><div v-for="index in 4" :key="index" class="skeleton-line"></div></div>
      <div v-else-if="pluginCalls.length" class="event-table">
        <div v-for="call in pluginCalls" :key="call.id" class="table-row plugin-call-row">
          <span class="update-title"><strong>{{ providerTitle(plugins.find((plugin) => plugin.id === call.pluginId) || { id: call.pluginId }) }}</strong><small>{{ call.capability }} · #{{ call.id }}</small></span>
          <span class="plugin-call-result"><span class="status-chip" :class="call.ok ? 'chip-processed' : 'chip-failed'">{{ call.ok ? '成功' : '失败' }}</span><small v-if="call.errorCode" class="plugin-call-error"><span>{{ pluginCallErrorLabel(call.errorCode) }}</span><code>{{ call.errorCode }}</code></small></span>
          <span class="table-secondary">{{ call.source }}</span>
          <span class="table-secondary">{{ call.durationMs }} ms</span>
          <span class="table-secondary">{{ formatDate(call.calledAt) }}</span>
          <span class="table-secondary call-correlation" :title="call.correlationId || ''">{{ call.correlationId || '—' }}</span>
        </div>
      </div>
      <div v-else class="empty-state compact-empty"><strong>还没有插件调用记录</strong><p>首次调用 provider 能力后，记录会显示在这里。</p></div>
      <div class="table-footer plugin-call-footer"><span>显示最近 {{ pluginCalls.length }} 条。日志不包含插件输入、响应或凭据。</span><button v-if="callsHasMore" class="button button-quiet button-small" type="button" :disabled="callsLoading" @click="loadCalls({ append: true })">{{ callsLoading ? '正在加载…' : '加载更多' }}</button><button v-else class="button button-quiet button-small" type="button" :disabled="callsLoading" @click="loadCalls()">{{ callsLoading ? '正在刷新…' : '刷新记录' }}</button></div>
    </section>
    <div class="inline-alert info-alert"><span class="alert-symbol">i</span><div><strong>凭据安全保存</strong><p>插件运行配置保存在数据库，密码类字段在服务端加密且不会回显。部署级命令路径仍由服务部署配置管理。</p></div></div>
  </div>
</template>
