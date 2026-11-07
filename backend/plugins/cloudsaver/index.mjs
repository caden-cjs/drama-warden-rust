const timeoutSignal = (timeoutMs) => AbortSignal.timeout(timeoutMs);

function codedError(code) {
  return Object.assign(new Error(code), { code });
}

export function createCloudSaverResourcePlugin({
  cloudsaverUrl = process.env.CLOUDSAVER_URL,
  cloudsaverUser = process.env.CLOUDSAVER_USER,
  cloudsaverPassword = process.env.CLOUDSAVER_PASS,
  fetchImpl = globalThis.fetch,
} = {}) {
  const baseUrl = String(cloudsaverUrl || '').trim().replace(/\/+$/, '');
  const username = String(cloudsaverUser || '');
  const password = String(cloudsaverPassword || '');
  let token = '';
  let expiresAt = 0;

  async function login() {
    if (!baseUrl || !username || !password) throw codedError('CLOUDSAVER_NOT_CONFIGURED');
    const response = await fetchImpl(`${baseUrl}/api/user/login`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username, password }),
      signal: timeoutSignal(8000),
    }).catch(() => { throw codedError('CLOUDSAVER_LOGIN_FAILED'); });
    if (!response.ok) throw codedError('CLOUDSAVER_LOGIN_FAILED');
    const body = await response.json().catch(() => { throw codedError('CLOUDSAVER_RESPONSE_INVALID'); });
    const nextToken = body?.data?.token;
    const parts = typeof nextToken === 'string' ? nextToken.split('.') : [];
    if (!body?.success || parts.length < 2) throw codedError('CLOUDSAVER_LOGIN_FAILED');
    let payload;
    try {
      payload = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8'));
    } catch {
      throw codedError('CLOUDSAVER_RESPONSE_INVALID');
    }
    token = nextToken;
    expiresAt = Number(payload?.exp) || 0;
    return token;
  }

  async function getToken() {
    const now = Math.floor(Date.now() / 1000);
    return token && expiresAt - now > 60 ? token : login();
  }

  async function search({ query, keyword, storageProvider = '', limit = 100 } = {}) {
    const searchTerm = String(query || keyword || '').trim();
    if (!searchTerm) throw codedError('RESOURCE_QUERY_REQUIRED');
    const authorization = await getToken();
    const url = new URL(`${baseUrl}/api/search`);
    url.searchParams.set('keyword', searchTerm);
    const response = await fetchImpl(url, {
      headers: { authorization: `Bearer ${authorization}`, accept: 'application/json' },
      signal: timeoutSignal(30_000),
    }).catch(() => { throw codedError('CLOUDSAVER_SEARCH_FAILED'); });
    if (!response.ok) throw codedError('CLOUDSAVER_SEARCH_FAILED');
    const result = await response.json().catch(() => { throw codedError('CLOUDSAVER_RESPONSE_INVALID'); });
    if (result?.success !== true) throw codedError('CLOUDSAVER_SEARCH_FAILED');
    const channels = Array.isArray(result?.data) ? result.data : [];
    const items = [];
    const requestedProvider = String(storageProvider || '').trim().toLowerCase();
    const targetProvider = requestedProvider === '115' ? 'pan115' : requestedProvider;
    for (const channel of channels) {
      const source = channel?.channelInfo || {};
      for (const entry of Array.isArray(channel?.list) ? channel.list : []) {
        for (const link of Array.isArray(entry?.cloudLinks) ? entry.cloudLinks : []) {
          const urlValue = String(link?.link || '').trim();
          const rawProviderId = String(link?.cloudType || '').trim().toLowerCase();
          const providerId = rawProviderId === '115' ? 'pan115' : rawProviderId;
          if (!urlValue || (targetProvider && targetProvider !== providerId)) continue;
          items.push({
            shareUrl: urlValue,
            title: String(entry?.title || ''),
            sourceId: String(source.id ?? channel?.id ?? ''),
            sourceName: String(source.name || channel?.channel || ''),
            storageProvider: providerId,
          });
          if (items.length >= Math.max(1, Math.min(Number(limit) || 100, 500))) {
            return { items, truncated: true };
          }
        }
      }
    }
    return { items, truncated: false };
  }

  return {
    async initialize() {},
    async invoke({ capability, input = {} }) {
      if (capability !== 'search') throw codedError('PLUGIN_CAPABILITY_UNSUPPORTED');
      return search(input);
    },
    async health() {
      if (!baseUrl || !username || !password) return { ok: false, configured: false };
      try {
        await getToken();
        return { ok: true, configured: true, authorized: true };
      } catch (error) {
        return { ok: false, configured: true, authorized: false, errorCode: error.code || 'CLOUDSAVER_LOGIN_FAILED' };
      }
    },
    async reset() {
      token = '';
      expiresAt = 0;
    },
  };
}
