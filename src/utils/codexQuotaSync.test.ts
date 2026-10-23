import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { CodexAccount } from '../types/codex.ts';
import { mergeCodexQuotaUpdate, type CodexQuotaUpdate } from './codexQuotaSync.ts';
import { isPremiumSeatTeamAccount } from './codexPremiumSeatEligibility';

const account = {
  id: 'local', account_id: 'workspace', organization_id: 'org', token_generation: 2,
  tokens: { access_token: 'fixture', id_token: 'fixture-id' }, account_note: 'user note',
  usage_updated_at: 100, quota_error: { message: 'old failure' },
} as CodexAccount;
const update = {
  account_id: 'local', remote_account_id: 'workspace', organization_id: 'org', token_generation: 2,
  usage_updated_at: 101, sequence: 1, quota: { hourly_percentage: 10, weekly_percentage: 50 }, plan_type: 'plus',
} as CodexQuotaUpdate;

test('a quota event updates one account immediately and retains credentials and user edits', () => {
  const merged = mergeCodexQuotaUpdate(account, update);
  assert.equal(merged.quota, update.quota);
  assert.equal(merged.tokens, account.tokens);
  assert.equal(merged.account_note, account.account_note);
  assert.equal(merged.quota_error, undefined);
  assert.equal(merged.usage_updated_at, 101);
});

test('late events for another account, workspace, organization or generation are rejected', () => {
  for (const patch of [
    { account_id: 'other' }, { remote_account_id: 'other-workspace' }, { organization_id: null },
    { token_generation: 1 }, { usage_updated_at: 99 },
  ]) assert.equal(mergeCodexQuotaUpdate(account, { ...update, ...patch }), account);
});

test('a delayed account-list read merges a newer quota event while preserving unrelated fields', () => {
  const pendingListResult = { ...account, account_note: 'new user note' };
  const merged = mergeCodexQuotaUpdate(pendingListResult, update);
  assert.equal(merged.quota, update.quota);
  assert.equal(merged.account_note, 'new user note');
  const fresherDiskRead = { ...account, usage_updated_at: 102 };
  assert.equal(mergeCodexQuotaUpdate(fresherDiskRead, update), fresherDiskRead);
});

test('a committed event from newer credentials can refresh quota without fabricating tokens', () => {
  const merged = mergeCodexQuotaUpdate(account, { ...update, token_generation: 3 });
  assert.equal(merged.quota, update.quota);
  assert.equal(merged.tokens, account.tokens);
  assert.equal(merged.token_generation, 2);
});

test('seat-specific quota plans retain the workspace identity confirmed by the backend', () => {
  const team = { ...account, user_id: 'member', plan_type: 'team' };
  assert.equal(isPremiumSeatTeamAccount(team), true);
  const merged = mergeCodexQuotaUpdate(team, { ...update, plan_type: 'prolite', account_structure: 'workspace' });
  assert.equal(merged.plan_type, 'prolite');
  assert.equal(merged.account_structure, 'workspace');
  assert.equal(isPremiumSeatTeamAccount(merged), true);
  assert.equal(merged.tokens, team.tokens);
});

test('quota metadata preserves old event compatibility but respects an explicit identity withdrawal', () => {
  const team = { ...account, account_structure: 'workspace' };
  assert.equal(mergeCodexQuotaUpdate(team, update).account_structure, 'workspace');
  assert.equal(mergeCodexQuotaUpdate(team, { ...update, account_structure: null }).account_structure, undefined);
  const personal = mergeCodexQuotaUpdate(team, { ...update, plan_type: 'team', account_structure: 'personal' });
  assert.equal(personal.account_structure, 'personal');
  assert.equal(isPremiumSeatTeamAccount({ ...personal, user_id: 'member' }), false);
});

test('quota structure never crosses identity guards or overwrites a same-quota profile reread', () => {
  const confirmed = { ...update, account_structure: 'workspace' };
  for (const patch of [{ remote_account_id: 'other' }, { token_generation: 1 }, { usage_updated_at: 99 }]) {
    assert.equal(mergeCodexQuotaUpdate(account, { ...confirmed, ...patch }), account);
  }
  const personalProfile = { ...account, usage_updated_at: update.usage_updated_at, account_structure: 'personal' };
  assert.equal(mergeCodexQuotaUpdate(personalProfile, confirmed, 'snapshot').account_structure, 'personal');
  const missingProfile = { ...account, usage_updated_at: update.usage_updated_at };
  assert.equal(mergeCodexQuotaUpdate(missingProfile, confirmed, 'snapshot').account_structure, 'workspace');
});

test('new live events in the same second can confirm or withdraw a workspace structure', () => {
  const pending = { ...account, user_id: 'member', usage_updated_at: update.usage_updated_at,
    plan_type: 'team', account_structure: 'unknown' };
  const confirmed = mergeCodexQuotaUpdate(pending, { ...update, plan_type: 'prolite', account_structure: 'workspace' });
  assert.equal(confirmed.account_structure, 'workspace');
  assert.equal(isPremiumSeatTeamAccount(confirmed), true);
  for (const account_structure of ['personal', null]) {
    const withdrawn = mergeCodexQuotaUpdate(confirmed, { ...update, plan_type: 'prolite', account_structure });
    assert.equal(withdrawn.account_structure, account_structure ?? undefined);
    assert.equal(isPremiumSeatTeamAccount(withdrawn), false);
  }
});
