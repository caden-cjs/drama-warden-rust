const timeoutSignal = (timeoutMs) => AbortSignal.timeout(timeoutMs);

function codedError(code) {
  return Object.assign(new Error(code), { code });
}

function normalizedTitle(value) {
  return String(value || '')
    .normalize('NFKC')
    .toLocaleLowerCase()
    .replace(/[\s\p{P}\p{S}]+/gu, '');
}

function comparableResourceTitle(value) {
  return String(value || '')
    .normalize('NFKC')
    .replace(/^\s*名称\s*[:：]\s*/u, '')
    .replace(/^\s*(?:🎬|📺)\s*/u, '')
    .replace(/^(?:电视剧|连续剧|电影|动漫|综艺)\s*[|｜:：]\s*/u, '')
    .trim();
}

function resourceRelevance(item, query, mediaType) {
  const rawTitle = String(item?.title || '');
  const title = normalizedTitle(rawTitle);
  const comparable = normalizedTitle(comparableResourceTitle(rawTitle));
  const normalizedQuery = normalizedTitle(query);
  let score = title.includes(normalizedQuery) ? 500 : 0;
  if (comparable === normalizedQuery) score = Math.max(score, 950);
  else if (comparable.startsWith(normalizedQuery)) score = Math.max(score, 800);
  else if (title.startsWith(normalizedQuery)) score = Math.max(score, 700);

  if (mediaType === 'tv') {
    if (/电视剧|连续剧|剧集|\bseries\b|\btv\b/i.test(rawTitle)) score += 500;
    if (/电影|\bmovie\b|\bfilm\b/i.test(rawTitle)) score -= 500;
  }
  return score;
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

  async function search({ query, keyword, aliases = [], mediaType = '', storageProvider = '', limit = 100 } = {}) {
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
    const seenUrls = new Set();
    const requestedProvider = String(storageProvider || '').trim().toLowerCase();
    const targetProvider = requestedProvider === '115' ? 'pan115' : requestedProvider;
    for (const channel of channels) {
      const source = channel?.channelInfo || {};
      for (const entry of Array.isArray(channel?.list) ? channel.list : []) {
        for (const link of Array.isArray(entry?.cloudLinks) ? entry.cloudLinks : []) {
          const urlValue = String(link?.link || '').trim();
          const rawProviderId = String(link?.cloudType || '').trim().toLowerCase();
          const providerId = rawProviderId === '115' ? 'pan115' : rawProviderId;
          if (!urlValue || seenUrls.has(urlValue) || (targetProvider && targetProvider !== providerId)) continue;
          seenUrls.add(urlValue);
          items.push({
            shareUrl: urlValue,
            title: String(entry?.title || ''),
            sourceId: String(source.id ?? channel?.id ?? ''),
            sourceName: String(source.name || channel?.channel || ''),
            storageProvider: providerId,
          });
        }
      }
    }
    const maxResults = Math.max(1, Math.min(Number(limit) || 100, 500));
    const stableQueries = [searchTerm, ...(Array.isArray(aliases) ? aliases : [])]
      .map(value => String(value || '').trim())
      .filter(Boolean);
    const ranked = items
      .map((item, index) => ({
        item,
        index,
        score: Math.max(...stableQueries.map(value => resourceRelevance(item, value, mediaType))),
      }))
      .sort((left, right) => right.score - left.score || left.index - right.index);
    return {
      items: ranked.slice(0, maxResults).map(entry => entry.item),
      truncated: ranked.length > maxResults,
    };
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
