import { createInterface } from 'node:readline';

const MAX_FRAME_BYTES = 24 * 1024 * 1024;

const definitions = {
  cloudsaver: {
    name: 'CloudSaver', label: 'CloudSaver', kind: 'resource-provider', version: '0.1.0',
    module: './cloudsaver/index.mjs', factory: 'createCloudSaverResourcePlugin',
    fields: [
      { key: 'cloudsaverUrl', type: 'url', required: true },
      { key: 'cloudsaverUser', type: 'text', required: true },
      { key: 'cloudsaverPassword', type: 'password', required: true },
    ],
  },
  'wps-query': {
    name: 'WPS query', label: 'WPS 自助资源搜索库', kind: 'resource-provider', version: '1.0.0',
    module: './wps-query/index.mjs', factory: 'createWpsQueryPlugin',
    fields: [{ key: 'wpsQueryUrl', type: 'url', required: true }],
  },
  'local-storage': {
    name: 'Local filesystem', label: '本地文件系统', kind: 'storage-provider', version: '0.1.0',
    module: './local-storage/index.mjs', factory: 'createLocalStoragePlugin',
    fields: [
      { key: 'storageRoot', type: 'text', required: true },
      { key: 'shareRoot', type: 'text', required: true },
    ],
  },
  'local-resource': {
    name: 'Local share search', label: '本地分享搜索', kind: 'resource-provider', version: '0.1.0',
    module: './local-resource/index.mjs', factory: 'createLocalResourcePlugin',
    fields: [{ key: 'shareRoot', type: 'text', required: true }],
  },
  quark: {
    name: 'Quark Cloud Drive', label: '夸克网盘', kind: 'storage-provider', version: '0.1.0',
    module: './quark/index.mjs', factory: 'createQuarkStoragePlugin',
    fields: [
      { key: 'cliPath', type: 'text', required: true },
      { key: 'homeDirectory', type: 'text', required: false },
      { key: 'stateDirectory', type: 'text', required: false },
      { key: 'recycleDirectory', type: 'text', required: false },
    ],
    removeModes: ['recycle'],
  },
  pan115: {
    name: '115 Cloud Drive', label: '115 网盘', kind: 'storage-provider', version: '0.1.0',
    module: './pan115/index.mjs', factory: 'createPan115StoragePlugin',
    fields: [
      { key: 'cliPath', type: 'text', required: false },
      { key: 'homeDirectory', type: 'text', required: false },
      { key: 'cookie', type: 'password', required: false },
      { key: 'minIntervalMs', type: 'text', required: false },
      { key: 'wafCooldownMs', type: 'text', required: false },
    ],
    removeModes: ['permanent'],
  },
  smartstrm: {
    name: 'SmartStrm', label: 'SmartStrm 索引', kind: 'indexer', version: '0.1.0',
    module: './indexer/smartstrm.mjs', factory: 'createSmartstrmIndexerPlugin',
    fields: [
      { key: 'baseUrl', type: 'url', required: true },
      { key: 'webhookToken', type: 'password', required: true },
      { key: 'taskName', type: 'text', required: false },
    ],
  },
  'feishu-notification': {
    name: 'Feishu notification provider', label: '飞书通知', kind: 'notification-provider', version: '0.1.0',
    module: './notification/feishu.mjs', factory: 'createFeishuNotificationPlugin',
    fields: [
      { key: 'webhookUrl', type: 'url', required: false },
      { key: 'webhookSecret', type: 'password', required: false },
      { key: 'eventWebhookUrl', type: 'url', required: false },
      { key: 'eventWebhookToken', type: 'password', required: false },
    ],
    subscriptions: ['workflow.event.status-changed'],
  },
  opensubtitles: {
    name: 'OpenSubtitles', label: 'OpenSubtitles', kind: 'subtitle-provider', version: '1.0.0',
    module: './subtitle/opensubtitles.mjs', factory: 'createOpenSubtitlesPlugin',
    fields: [
      { key: 'apiKey', type: 'password', required: true },
      { key: 'userAgent', type: 'text', required: false },
    ],
  },
  assrt: {
    name: 'Assrt', label: '伪射手字幕网', kind: 'subtitle-provider', version: '1.0.0',
    module: './subtitle/assrt.mjs', factory: 'createAssrtPlugin', fields: [],
  },
};

let initializedPlugin = null;

function manifestFor(id, definition) {
  let capabilityNames;
  if (definition.kind === 'storage-provider') {
    capabilityNames = [
      'list', 'listSharedFiles', 'save', 'move', 'mkdir', 'rename', 'renameMany', 'remove', 'clean', 'status',
      ...(id === 'local-storage' ? ['writeSidecar'] : []),
    ];
  } else if (definition.kind === 'resource-provider') capabilityNames = ['search'];
  else if (definition.kind === 'subtitle-provider') capabilityNames = ['search', 'download'];
  else if (definition.kind === 'indexer') capabilityNames = ['reindex'];
  else capabilityNames = ['notifyStatusEvent'];

  const capabilityDomain = {
    'storage-provider': 'storage',
    'resource-provider': 'resource',
    'subtitle-provider': 'subtitle',
    indexer: 'indexer',
    'notification-provider': 'notification',
  }[definition.kind];
  const result = {
    protocolVersion: 1,
    apiVersion: 2,
    id,
    name: definition.name,
    label: definition.label,
    kind: definition.kind,
    version: definition.version,
    capabilities: capabilityNames.map((name) => ({ name, domain: capabilityDomain })),
    subscriptions: definition.subscriptions || [],
    configSchema: {
      version: 1,
      fields: definition.fields.map(({ key, type, required }) => ({ key, type, required })),
    },
  };
  if (definition.kind === 'storage-provider') {
    result.storageContract = {
      version: 2,
      coreCapabilities: [
        'list', 'listSharedFiles', 'save', 'move', 'mkdir', 'rename', 'renameMany', 'remove', 'clean', 'status',
      ],
      removeModes: definition.removeModes || ['permanent'],
    };
  }
  return result;
}

function setPluginEnvironment(id, config) {
  const aliases = {
    'local-storage': { storageRoot: 'LOCAL_STORAGE_ROOT', shareRoot: 'LOCAL_STORAGE_SHARE_ROOT' },
    'local-resource': { shareRoot: 'LOCAL_STORAGE_SHARE_ROOT' },
    quark: {
      cliPath: 'QUARK_CLI_PATH', homeDirectory: 'HOME', stateDirectory: 'QUARK_STATE_DIR', recycleDirectory: 'QUARK_RECYCLE_DIR',
    },
    pan115: {
      cliPath: 'PAN115_CLI_PATH', homeDirectory: 'HOME', cookie: 'PAN115_COOKIE', minIntervalMs: 'PAN115_MIN_INTERVAL_MS',
      wafCooldownMs: 'PAN115_WAF_COOLDOWN_MS',
    },
    smartstrm: {
      baseUrl: 'SMARTSTRM_BASE_URL', webhookToken: 'SMARTSTRM_WEBHOOK_TOKEN', taskName: 'SMARTSTRM_TASK_NAME',
    },
    cloudsaver: {
      cloudsaverUrl: 'CLOUDSAVER_URL', cloudsaverUser: 'CLOUDSAVER_USER', cloudsaverPassword: 'CLOUDSAVER_PASS',
    },
    'wps-query': { wpsQueryUrl: 'WPS_QUERY_URL' },
    'feishu-notification': {
      webhookUrl: 'FEISHU_WEBHOOK_URL', webhookSecret: 'FEISHU_WEBHOOK_SECRET',
      eventWebhookUrl: 'UPDATE_EVENT_WEBHOOK_URL', eventWebhookToken: ['UPDATE_EVENT_WEBHOOK', 'TOKEN'].join('_'),
    },
    opensubtitles: { apiKey: 'OPENSUBTITLES_API_KEY', userAgent: 'OPENSUBTITLES_USER_AGENT' },
  }[id] || {};
  for (const [key, value] of Object.entries(config || {})) {
    const variable = aliases[key];
    if (variable) process.env[variable] = String(value);
  }
}

async function createPlugin(id, definition, config) {
  setPluginEnvironment(id, config);
  const module = await import(new URL(definition.module, import.meta.url));
  const factory = module[definition.factory];
  if (typeof factory !== 'function') throw Object.assign(new Error('plugin factory is missing'), { code: 'PLUGIN_FACTORY_MISSING' });
  const configured = Object.fromEntries(Object.entries(config || {}).map(([key, value]) => [key, String(value)]));
  let plugin;
  if (id === 'local-storage') {
    plugin = factory({ storageRoot: configured.storageRoot, shareRoot: configured.shareRoot });
  } else if (id === 'local-resource') {
    plugin = factory({ shareRoot: configured.shareRoot });
  } else if (id === 'smartstrm') {
    plugin = factory({ baseUrl: configured.baseUrl, webhookToken: configured.webhookToken, taskName: configured.taskName });
  } else if (id === 'feishu-notification') {
    plugin = factory({
      webhookUrl: configured.webhookUrl,
      webhookSecret: configured.webhookSecret,
      eventWebhookUrl: configured.eventWebhookUrl,
      eventWebhookToken: configured.eventWebhookToken,
    });
  } else if (id === 'opensubtitles') {
    plugin = factory({ apiKey: configured.apiKey, userAgent: configured.userAgent });
  } else if (id === 'cloudsaver') {
    plugin = factory({
      cloudsaverUrl: configured.cloudsaverUrl,
      cloudsaverUser: configured.cloudsaverUser,
      cloudsaverPassword: configured.cloudsaverPassword,
    });
  } else {
    plugin = factory();
  }
  return plugin;
}

function normalizedError(error) {
  const code = String(error?.code || 'PLUGIN_ERROR');
  return {
    code: /^[A-Z0-9_]{1,64}$/.test(code) ? code : 'PLUGIN_ERROR',
    sideEffectMayHaveCompleted: Boolean(error?.sideEffectMayHaveCompleted),
  };
}

async function handle(id, method, input) {
  const definition = definitions[id];
  if (!definition) throw Object.assign(new Error('unknown plugin'), { code: 'PLUGIN_UNKNOWN' });
  if (method === 'manifest') return manifestFor(id, definition);
  if (method === 'initialize') {
    initializedPlugin = await createPlugin(id, definition, input?.config || {});
    await initializedPlugin.initialize?.({ config: input?.config || {} });
    return { ok: true };
  }
  if (!initializedPlugin) throw Object.assign(new Error('plugin is not initialized'), { code: 'PLUGIN_NOT_INITIALIZED' });
  if (method === 'reset') {
    await initializedPlugin.reset?.();
    return { ok: true };
  }
  if (method === 'health') return (await initializedPlugin.health?.()) || { ok: true };
  if (method === 'monitor') return (await initializedPlugin.monitor?.(input || {})) || { ok: true };
  if (method === 'invoke') {
    return initializedPlugin.invoke({
      capability: input?.capability,
      input: input?.input || {},
      context: input?.context || {},
    });
  }
  if (method === 'onEvent') {
    return initializedPlugin.onEvent(input?.event || {}, input?.context || {});
  }
  throw Object.assign(new Error('unsupported plugin method'), { code: 'PLUGIN_METHOD_UNSUPPORTED' });
}

const id = process.argv[2];
if (!definitions[id]) process.exitCode = 2;
else {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  for await (const line of lines) {
    let request;
    try {
      if (Buffer.byteLength(line) > MAX_FRAME_BYTES) throw Object.assign(new Error('frame too large'), { code: 'PLUGIN_REQUEST_TOO_LARGE' });
      request = JSON.parse(line);
      const result = await handle(id, request.method, request.input || {});
      process.stdout.write(JSON.stringify({ requestId: request.requestId, result }) + '\n');
    } catch (error) {
      process.stdout.write(JSON.stringify({
        requestId: request?.requestId || '',
        error: normalizedError(error),
      }) + '\n');
    }
  }
}
