import assert from 'node:assert/strict';
import test from 'node:test';
import { loadHookModule } from '../../tests/helpers/reactHookHarness';

test('seat controls pass an explicit forced target without changing ordinary actions', async () => {
  const calls: any[] = [];
  const state = { phase: 'ready' };
  const h = loadHookModule(new URL('./codexPremiumSeatService.ts', import.meta.url), {
    '@tauri-apps/api/core': { invoke: async (...args: unknown[]) => { calls.push(args); return state; } },
  });
  const confirmation = { holderAccountId: 'source', revision: 7, resetAt: 1_800_003_600 };
  assert.equal(await h.exports.controlPremiumSeat('force_next', 'member', confirmation), state);
  assert.equal(calls[0][0], 'codex_premium_seat_control');
  assert.equal(calls[0][1].action, 'force_next');
  assert.equal(calls[0][1].targetAccountId, 'member');
  assert.deepEqual(calls[0][1].confirmation, confirmation);
  await h.exports.controlPremiumSeat('next');
  assert.equal(calls[1][1].action, 'next');
  assert.equal(calls[1][1].targetAccountId, undefined);
  assert.equal(calls[1][1].confirmation, undefined);
});
