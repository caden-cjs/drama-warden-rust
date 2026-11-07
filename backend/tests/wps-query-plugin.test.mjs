import test from 'node:test';
import assert from 'node:assert/strict';
import { createWpsQueryPlugin } from '../plugins/wps-query/index.mjs';

const appUrl = 'https://web.wps.cn/etapps/query/q/ki7rF3kM';

test('WPS resource plugin validates the public app and checks its campaign', async () => {
  const requests = [];
  const plugin = createWpsQueryPlugin({
    appUrl,
    fetchImpl: async (url) => {
      requests.push(String(url));
      if (String(url).endsWith('/config')) {
        return Response.json({ code: 0, data: { encrypt: { aes_key: Buffer.alloc(16).toString('base64') } } });
      }
      return Response.json({ code: 0, data: { name: 'Public query', setting: { enquiryPageSetting: { conditions: [] } } } });
    },
  });

  const health = await plugin.health();
  assert.equal(health.ok, true);
  assert.deepEqual(requests, [
    'https://f-api.wps.cn/ksenquiry/api/v3/config',
    'https://f-api.wps.cn/ksenquiry/api/v3/campaign/ki7rF3kM',
  ]);
  assert.equal(plugin.manifest.id, 'wps-query');
  assert.equal(plugin.manifest.configSchema.fields[0].key, 'wpsQueryUrl');
  assert.ok(plugin.capabilities.includes('search'));
});

test('WPS resource plugin rejects application URLs outside the public WPS query format', async () => {
  const plugin = createWpsQueryPlugin({ appUrl: 'https://example.com/private' });
  await assert.rejects(plugin.health(), /必须是 https:\/\/web\.wps\.cn/);
});
