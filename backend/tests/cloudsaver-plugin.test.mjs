import assert from 'node:assert/strict';
import test from 'node:test';
import { createCloudSaverResourcePlugin } from '../plugins/cloudsaver/index.mjs';

test('CloudSaver resource plugin returns only matching share links in Rust resource shape', async () => {
  const requests = [];
  const plugin = createCloudSaverResourcePlugin({
    cloudsaverUrl: 'https://cloudsaver.example/',
    cloudsaverUser: 'user',
    cloudsaverPassword: 'password',
    fetchImpl: async (input, init = {}) => {
      const url = new URL(input);
      requests.push({ url, init });
      if (url.pathname === '/api/user/login') {
        const jwtPayload = Buffer.from(JSON.stringify({ exp: Math.floor(Date.now() / 1000) + 3600 })).toString('base64url');
        return Response.json({ success: true, data: { token: `header.${jwtPayload}.signature` } });
      }
      return Response.json({
        success: true,
        data: [{
          channelInfo: { id: 12, name: 'channel' },
          list: [{
            title: 'Sample show S01E02',
            cloudLinks: [
              { link: 'https://share.example/quark', cloudType: 'quark' },
              { link: 'https://share.example/115', cloudType: '115' },
            ],
          }],
        }],
      });
    },
  });

  const result = await plugin.invoke({
    capability: 'search',
    input: { query: 'Sample show', storageProvider: 'pan115', limit: 5 },
  });
  assert.deepEqual(result, {
    items: [{
      shareUrl: 'https://share.example/115',
      title: 'Sample show S01E02',
      sourceId: '12',
      sourceName: 'channel',
      storageProvider: 'pan115',
    }],
    truncated: false,
  });
  assert.equal(requests.length, 2);
  assert.equal(requests[1].url.searchParams.get('keyword'), 'Sample show');
  assert.match(requests[1].init.headers.authorization, /^Bearer header\.[A-Za-z0-9_-]+\.signature$/);
});
