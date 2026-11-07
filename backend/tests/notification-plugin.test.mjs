import assert from 'node:assert/strict';
import test from 'node:test';
import { createFeishuNotificationPlugin } from '../plugins/notification/feishu.mjs';

test('status notification uses the configured event webhook and keeps the envelope safe', async () => {
  const requests = [];
  const plugin = createFeishuNotificationPlugin({
    webhookUrl: '',
    eventWebhookUrl: 'https://events.example/notify',
    eventWebhookToken: 'event-token',
    fetchImpl: async (url, options) => {
      requests.push({ url: String(url), options });
      return { ok: true };
    },
  });

  await plugin.onEvent({
    type: 'workflow.event.status-changed',
    id: 42,
    showId: 7,
    correlationId: 'corr-1',
    payload: { status: 'processed', kind: 'new_ep', seasonNumber: 2, episodeNumbers: [9] },
  });

  assert.equal(requests.length, 1);
  assert.equal(requests[0].url, 'https://events.example/notify');
  assert.equal(requests[0].options.headers.authorization, 'Bearer event-token');
  const body = JSON.parse(requests[0].options.body);
  assert.equal(body.event, 'workflow.event.status-changed');
  assert.equal(body.payload.episodeNumbers[0], 9);
  assert.equal(JSON.stringify(body).includes('event-token'), false);
});
