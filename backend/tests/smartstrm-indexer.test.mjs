import test from 'node:test';
import assert from 'node:assert/strict';
import { createSmartstrmIndexerPlugin } from '../plugins/indexer/smartstrm.mjs';

test('uses the per-show task name supplied by the Rust workflow', async () => {
  let request;
  const plugin = createSmartstrmIndexerPlugin({
    baseUrl: 'https://smartstrm.example',
    webhookToken: 'test-token',
    fetchImpl: async (url, init) => {
      request = { url: String(url), body: JSON.parse(init.body) };
      return new Response(JSON.stringify({ success: true }), { status: 200 });
    },
  });

  const result = await plugin.invoke({ capability: 'reindex', input: { taskName: 'quark_动漫' } });
  assert.equal(result.ok, true);
  assert.equal(result.taskName, 'quark_动漫');
  assert.equal(request.body.task.name, 'quark_动漫');
  assert.equal(request.url, 'https://smartstrm.example/webhook/test-token');
  assert.equal(plugin.manifest.configSchema.fields.find((field) => field.key === 'taskName').required, false);
});
