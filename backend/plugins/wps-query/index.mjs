import crypto from "node:crypto"
import { definePlugin } from "../contract.mjs"

const API_BASE = "https://f-api.wps.cn"
const API_PATH = "/ksenquiry/api/v3"
const CONFIG_URL = `${API_BASE}${API_PATH}/config`
const CONFIG_CACHE_MS = 60 * 60_000
const CAMPAIGN_CACHE_MS = 5 * 60_000
const REQUEST_TIMEOUT_MS = 15_000
const MAX_RESPONSE_BYTES = 8 * 1024 * 1024

// These public-client signing identifiers are also present in the WPS query
// application's published JavaScript. They are protocol identifiers, not a
// user's password, session, or login token.
function rotateLettersBack(value, distance = 4) {
  return [...value].map(char => {
    const code = char.charCodeAt(0)
    if (code >= 65 && code <= 90) return String.fromCharCode((code - 65 - distance + 26) % 26 + 65)
    if (code >= 97 && code <= 122) return String.fromCharCode((code - 97 - distance + 26) % 26 + 97)
    return char
  }).join("")
}

const SIGNING_FORM_ID = rotateLettersBack("txoESJhwSJ" + "fzq9uUV50MLqZMYQgWWa3zwUl")
const SIGNING_KEY_ID = rotateLettersBack("VIJSVQ202006" + "166Y9V63QVZ")

function parseAppUrl(value) {
  let parsed
  try { parsed = new URL(value) } catch { throw new Error("WPS 查询应用地址无效") }
  const match = parsed.protocol === "https:" && parsed.hostname === "web.wps.cn"
    ? parsed.pathname.match(/^\/etapps\/query\/q\/([A-Za-z0-9_-]+)\/?$/)
    : null
  if (!match || parsed.username || parsed.password) {
    throw new Error("WPS 查询应用地址必须是 https://web.wps.cn/etapps/query/q/{应用ID}")
  }
  return { shareId: match[1], pageUrl: `https://web.wps.cn/etapps/query/q/${match[1]}` }
}

function encryptJson(value, key, randomBytes) {
  const iv = randomBytes(16)
  const cipher = crypto.createCipheriv("aes-128-ofb", key, iv)
  cipher.setAutoPadding(false)
  const encrypted = Buffer.concat([cipher.update(JSON.stringify(value), "utf8"), cipher.final()])
  return { body: encrypted.toString("base64"), iv: iv.toString("base64") }
}

function decryptJson(value, key, iv) {
  const decipher = crypto.createDecipheriv("aes-128-ofb", key, Buffer.from(iv, "base64"))
  decipher.setAutoPadding(false)
  const plaintext = Buffer.concat([decipher.update(Buffer.from(value, "base64")), decipher.final()]).toString("utf8")
  return JSON.parse(plaintext)
}

function signatureFor(path, date) {
  const contentMd5 = crypto.createHash("md5").update(path).digest("hex")
  const signature = crypto.createHash("sha1").update(SIGNING_FORM_ID + contentMd5 + date).digest("hex")
  return { contentMd5, authorization: `WPS-2:${SIGNING_KEY_ID}:${signature}` }
}

async function readResponse(response, key, path) {
  const body = await response.text()
  if (Buffer.byteLength(body) > MAX_RESPONSE_BYTES) throw new Error("WPS 查询响应超过大小限制")

  let result
  try {
    const iv = response.headers.get("x-encrypt-iv")
    result = iv ? decryptJson(body, key, iv) : JSON.parse(body)
  } catch {
    throw new Error(`WPS 查询响应无法解析（HTTP ${response.status}）`)
  }

  if (!response.ok) {
    throw new Error(`WPS 查询请求失败（HTTP ${response.status}）：${result?.result || result?.message || path}`)
  }
  if (result?.code !== 0) {
    throw new Error(`WPS 查询失败（${result?.code ?? "unknown"}）：${result?.result || "应用返回异常"}`)
  }
  return result
}

function findLinks(value) {
  const found = new Set()
  const visit = item => {
    if (typeof item === "string") {
      for (const match of item.matchAll(/https?:\/\/[^\s<>"'`]+/gi)) {
        const candidate = match[0].replace(/[),.;，。；）】]+$/u, "")
        try {
          const parsed = new URL(candidate)
          if (parsed.protocol === "https:" && parsed.hostname) found.add(parsed.toString())
        } catch { /* ignore malformed document values */ }
      }
      return
    }
    if (Array.isArray(item)) { item.forEach(visit); return }
    if (item && typeof item === "object") Object.values(item).forEach(visit)
  }
  visit(value)
  return [...found]
}

function storageProviderFor(url) {
  const host = new URL(url).hostname.toLowerCase()
  const providers = [
    ["quark.cn", "quark"], ["baidu.com", "baidu"], ["drive.uc.cn", "uc"],
    ["115.com", "pan115"], ["aliyundrive.com", "alipan"], ["alipan.com", "alipan"],
    ["123pan.com", "123pan"], ["xunlei.com", "xunlei"], ["pikpak.com", "pikpak"],
  ]
  return providers.find(([domain]) => host === domain || host.endsWith(`.${domain}`))?.[1] || "unknown"
}

function collectStrings(value, output = []) {
  if (typeof value === "string") output.push(value.trim())
  else if (Array.isArray(value)) value.forEach(item => collectStrings(item, output))
  else if (value && typeof value === "object") Object.values(value).forEach(item => collectStrings(item, output))
  return output.filter(Boolean)
}

function normalizeDate(value) {
  const match = String(value || "").match(/^(\d{4})(\d{2})(\d{2})/)
  if (match) return `${match[1]}-${match[2]}-${match[3]}`
  const timestamp = Number(value)
  return Number.isFinite(timestamp) && timestamp > 0 ? new Date(timestamp).toISOString().slice(0, 10) : ""
}

function createItems(records, keyword, sourceId, sourceName) {
  if (!Array.isArray(records)) return []
  const normalizedKeyword = keyword.toLocaleLowerCase().replace(/\s+/gu, "")

  return records.flatMap((record, index) => {
    const fields = record?.fields && typeof record.fields === "object" ? record.fields : {}
    const strings = collectStrings(fields).filter(value => !/^https?:\/\//i.test(value))
    const title = strings
      .filter(value => value.toLocaleLowerCase().replace(/\s+/gu, "").includes(normalizedKeyword))
      .sort((a, b) => b.length - a.length)[0] || strings[0] || ""
    if (!title) return []

    const links = findLinks(fields).map(url => ({ url, storageProvider: storageProviderFor(url) }))
    const metadata = [...new Set(strings.filter(value =>
      value !== title && value.length <= 100 && !/^\d{10,}$/.test(value)
    ))]
    const published = normalizeDate(record?.commitTime) || normalizeDate(fields.H) || normalizeDate(record?.createdAt)

    return [{
      id: String(record?.id ?? index),
      title,
      ...(published ? { pubDate: published } : {}),
      content: metadata.join(" · "),
      tags: metadata,
      sourceId,
      sourceName,
      links,
    }]
  })
}

export function createWpsQueryPlugin({
  appUrl = process.env.WPS_QUERY_URL || "https://web.wps.cn/etapps/query/q/ki7rF3kM",
  fetchImpl = globalThis.fetch,
  now = Date.now,
  randomBytes = crypto.randomBytes,
  timeoutMs = REQUEST_TIMEOUT_MS,
} = {}) {
  let cachedKey
  let keyExpiresAt = 0
  let keyRequest
  const campaigns = new Map()

  async function getEncryptionKey() {
    if (cachedKey && keyExpiresAt > now()) return cachedKey
    if (keyRequest) return keyRequest
    keyRequest = (async () => {
      const response = await fetchImpl(CONFIG_URL, {
        headers: { accept: "application/json" },
        signal: AbortSignal.timeout(timeoutMs),
      })
      if (!response.ok) throw new Error(`WPS 配置读取失败（HTTP ${response.status}）`)
      const body = await response.json()
      const encodedKey = body?.data?.encrypt?.aes_key
      const key = typeof encodedKey === "string" ? Buffer.from(encodedKey, "base64") : Buffer.alloc(0)
      if (body?.code !== 0 || key.length !== 16) throw new Error("WPS 查询加密配置无效")
      cachedKey = key
      keyExpiresAt = now() + CONFIG_CACHE_MS
      return key
    })()
    try { return await keyRequest }
    finally { keyRequest = undefined }
  }

  async function requestApi({ shareId, pageUrl, path, method = "GET", body, queryToken }) {
    const key = await getEncryptionKey()
    const date = String(now())
    const { contentMd5, authorization } = signatureFor(path, date)
    const headers = {
      accept: "application/json",
      "content-type": "application/json; charset=utf-8",
      "x-date": date,
      "content-md5": contentMd5,
      authorization,
      referer: pageUrl,
    }
    if (queryToken) headers["x-query-token"] = queryToken

    let requestBody
    if (body !== undefined) {
      const encrypted = encryptJson(body, key, randomBytes)
      headers["x-encrypt-alg"] = "aes-128-ofb"
      headers["x-encrypt-iv"] = encrypted.iv
      requestBody = encrypted.body
    }

    const response = await fetchImpl(`${API_BASE}${path}`, {
      method,
      headers,
      ...(requestBody !== undefined ? { body: requestBody } : {}),
      signal: AbortSignal.timeout(timeoutMs),
    })
    return readResponse(response, key, path)
  }

  async function getCampaign(shareId, pageUrl) {
    const cached = campaigns.get(shareId)
    if (cached && cached.expiresAt > now()) return cached.data
    const data = (await requestApi({
      shareId,
      pageUrl,
      path: `/ksenquiry/api/v3/campaign/${encodeURIComponent(shareId)}`,
    }))?.data
    if (!data?.name || !Array.isArray(data?.setting?.enquiryPageSetting?.conditions)) {
      throw new Error("WPS 查询应用结构无效或已不再公开")
    }
    campaigns.set(shareId, { data, expiresAt: now() + CAMPAIGN_CACHE_MS })
    return data
  }

  async function health() {
    const { shareId, pageUrl } = parseAppUrl(appUrl)
    const campaign = await getCampaign(shareId, pageUrl)
    return { ok: true, detail: `WPS 查询应用可访问：${campaign.name}` }
  }

  async function search({ keyword } = {}) {
    const query = typeof keyword === "string" ? keyword.trim() : ""
    if (!query) return { items: [], sources: [] }

    const { shareId, pageUrl } = parseAppUrl(appUrl)
    const campaign = await getCampaign(shareId, pageUrl)
    const conditions = campaign.setting.enquiryPageSetting.conditions
    let searchConditionUsed = false
    const verifies = conditions.map(condition => {
      if (condition.matchType === "select" || /select/i.test(condition.dbType || "")) return { itemSelect: [] }
      if (condition.dateRange) return { itemDate: [] }
      if (!searchConditionUsed && (condition.matchType === "contain" || /text/i.test(condition.dbType || ""))) {
        searchConditionUsed = true
        return { item: query }
      }
      return { item: "" }
    })
    if (!searchConditionUsed) throw new Error("WPS 查询应用没有可用的文本搜索字段")

    const basePath = `/ksenquiry/api/v3/campaign/${encodeURIComponent(shareId)}`
    const verification = await requestApi({
      shareId,
      pageUrl,
      method: "POST",
      path: `${basePath}/verify`,
      body: { verifies, captchaParam: "", _t: now() },
    })
    const queryToken = verification?.data?.token
    if (typeof queryToken !== "string" || !queryToken) throw new Error("WPS 查询验证未返回访问令牌")

    const result = await requestApi({
      shareId,
      pageUrl,
      method: "POST",
      path: `${basePath}/query`,
      queryToken,
      body: { verifies, pageNum: 1, pageSize: 100, _t: now() },
    })
    const sourceId = `wps-query:${shareId}`
    return {
      items: createItems(result?.data, query, sourceId, campaign.name),
      sources: [{ id: sourceId, name: campaign.name }],
    }
  }

  return definePlugin({
    manifest: {
      id: "wps-query",
      name: "WPS 自助资源搜索库",
      label: "WPS 自助资源搜索库",
      kind: "resource-provider",
      version: "1.0.0",
      configSchema: {
        version: 1,
        fields: [{ key: "wpsQueryUrl", label: "WPS 查询应用地址", type: "url", required: true }],
      },
    },
    actions: { search },
    capabilityDomains: { search: ["resource"] },
    async health() { return health() },
    reset() {
      cachedKey = undefined
      keyExpiresAt = 0
      keyRequest = undefined
      campaigns.clear()
    },
  })
}

const plugin = createWpsQueryPlugin()
export const manifest = plugin.manifest
export const provider = plugin
export default plugin
