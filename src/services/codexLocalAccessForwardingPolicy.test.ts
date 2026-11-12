import assert from 'node:assert/strict';
import test from 'node:test';
import { loadHookModule } from '../../tests/helpers/reactHookHarness';

test('forwarding policy bridge preserves the mode, selected IDs and confirmed state', async () => {
  const calls: any[] = [];
  const state = { collection: {} };
  const h = loadHookModule(new URL('./codexLocalAccessService.ts', import.meta.url), {
    '@tauri-apps/api/core': { invoke: async (...args: unknown[]) => { calls.push(args); return state; } },
  });
  for (const mode of ['all', 'premium_only', 'selected_accounts']) {
    const policy = { mode, accountIds: ['a', 'b'] };
    assert.equal(await h.exports.updateCodexLocalAccessForwardingPolicy(policy), state);
    assert.equal(calls[calls.length - 1][0], 'codex_local_access_update_forwarding_policy');
    assert.equal(calls[calls.length - 1][1].policy, policy);
  }
});
