export function formatDate(value) {
  if (!value) return '—';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  return new Intl.DateTimeFormat('zh-CN', {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  }).format(date);
}

export function statusLabel(status) {
  return ({
    pending: '待处理',
    processing: '处理中',
    processed: '已完成',
    skipped: '已跳过',
    failed: '失败',
  })[status] || status || '未知';
}

export function statusTagType(status) {
  if (['processed', 'completed', 'success'].includes(status)) return 'success';
  if (['failed', 'error'].includes(status)) return 'error';
  if (['pending', 'uncertain'].includes(status)) return 'warning';
  if (status === 'processing') return 'info';
  return 'default';
}

export function mediaTypeLabel(type) {
  return type === 'movie' ? '电影' : '剧集';
}

export function providerTitle(provider) {
  return provider?.label || provider?.name || provider?.id || '未命名插件';
}

export function eventTitle(event) {
  const payload = event?.payload || {};
  return payload.shareTitle || payload.title || event?.kind || '追更事件';
}

const sensitivePayloadKey = /url|uri|link|token|cookie|password|secret|authorization/i;
const payloadLabels = {
  estimatedBytes: '预计大小',
  providerId: '存储提供方',
  seasonNumber: '季',
  selectedFiles: '所选文件',
  selectedFileCount: '文件数',
  storageId: '存储位置',
  shareTitle: '分享名称',
  showName: '节目',
  episodeNumbers: '集数',
};

function formatBytes(value) {
  if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return String(value);
  if (value < 1024) return `${value} B`;
  const units = ['KiB', 'MiB', 'GiB', 'TiB'];
  let size = value;
  let unit = -1;
  do {
    size /= 1024;
    unit += 1;
  } while (size >= 1024 && unit < units.length - 1);
  return `${size.toFixed(2)} ${units[unit]}`;
}

function formatPayloadValue(value, key = '') {
  if (key === 'size' || key === 'estimatedBytes') return formatBytes(value);
  if (Array.isArray(value)) return value.map((item) => formatPayloadValue(item)).join('；');
  if (value && typeof value === 'object') {
    const parts = [];
    if (Number.isInteger(value.episodeNumber)) parts.push(`第 ${value.episodeNumber} 集`);
    const title = value.name || value.title;
    if (typeof title === 'string' && title) parts.push(title);
    if (typeof value.size === 'number') parts.push(formatBytes(value.size));
    if (parts.length) return parts.join(' · ');

    return Object.entries(value)
      .filter(([nestedKey, nestedValue]) => nestedValue != null && !sensitivePayloadKey.test(nestedKey))
      .map(([nestedKey, nestedValue]) => `${payloadLabels[nestedKey] || nestedKey}: ${formatPayloadValue(nestedValue, nestedKey)}`)
      .join('；');
  }
  return value == null ? '' : String(value);
}

export function safePayloadEntries(payload = {}, limit = 12) {
  return Object.entries(payload)
    .filter(([key, value]) => value != null && !sensitivePayloadKey.test(key))
    .slice(0, limit)
    .map(([key, value]) => [payloadLabels[key] || key, formatPayloadValue(value, key)]);
}

export function errorMessage(error) {
  return error?.message || '请求失败，请稍后重试';
}
