import assert from 'node:assert/strict';
import test from 'node:test';
import type { CodexAccount, CodexQuota } from '../types/codex.ts';
import { createCodexOverviewAccountComparator, isCodexOverviewAccountAbnormal } from './codexAccountOverview.ts';

const account = (id: string, quota?: Partial<CodexQuota>) => ({
  id, created_at: 1, auth_mode: 'oauth', quota,
} as CodexAccount);
const compare = (sortBy: string, sortDirection: 'asc' | 'desc' = 'asc') =>
  createCodexOverviewAccountComparator({ sortBy, sortDirection,
    customSortOrder: [], currentAccountId: null, resolveSubscriptionTimestamp: () => null });

test('weekly sorting recognizes the weekly primary window used by free accounts', () => {
  const free = account('free', { hourly_percentage: 70, hourly_reset_time: 200,
    hourly_window_minutes: 10080, hourly_window_present: true, weekly_window_present: false });
  const plus = account('plus', { hourly_percentage: 100, weekly_percentage: 30,
    weekly_reset_time: 300 });
  assert.ok(compare('weekly')(free, plus) > 0);
  assert.ok(compare('weekly', 'desc')(free, plus) < 0);
  assert.ok(compare('weekly_reset')(free, plus) < 0);
  assert.ok(compare('weekly_reset', 'desc')(free, plus) > 0);
  assert.ok(compare('hourly')(free, plus) > 0);
});

test('missing windows stay after known zero quota in either direction', () => {
  const missing = account('missing');
  const zero = account('zero', { hourly_percentage: 0, weekly_percentage: 0 });
  for (const direction of ['asc', 'desc'] as const) {
    assert.ok(compare('weekly', direction)(missing, zero) > 0);
    assert.ok(compare('hourly', direction)(missing, zero) > 0);
  }
});

test('weekly exhaustion sorts using the same effective hourly quota displayed in cards', () => {
  const exhausted = account('exhausted', { hourly_percentage: 100, weekly_percentage: 0 });
  const available = account('available', { hourly_percentage: 50, weekly_percentage: 50 });
  assert.ok(compare('hourly')(exhausted, available) < 0);
});

test('access-token expiration alone is not an authorization-failure filter result', () => {
  const value = account('expired-access');
  for (const message of [
    'API 返回错误 401 Unauthorized',
    '刷新失败：API 返回错误 401 Unauthorized [error_code:token_expired] [body:{"message":"Your authentication token has expired. Please try refreshing it."}]',
    'Token 已过期且刷新失败: error sending request',
  ]) {
    value.quota_error = { code: 'token_expired', message, timestamp: 1 };
    assert.equal(isCodexOverviewAccountAbnormal(value), false, message);
  }
});

test('confirmed unrecoverable authorization still appears in the failure filter', () => {
  const value = account('terminal');
  for (const code of ['refresh_token_expired', 'refresh_token_invalidated', 'token_invalidated', 'token_revoked', 'invalid_grant', 'deactivated_workspace', 'codex_quota_auth_recovery_exhausted']) {
    value.quota_error = { code, message: 'request rejected', timestamp: 1 };
    assert.equal(isCodexOverviewAccountAbnormal(value), true, code);
  }
  value.quota_error = { code: 'token_expired', message: 'API 返回错误 401 Unauthorized', timestamp: 1 };
  value.requires_reauth = true;
  value.reauth_reason = 'CODEX_QUOTA_AUTH_RECOVERY_EXHAUSTED';
  assert.equal(isCodexOverviewAccountAbnormal(value), true);
  value.reauth_reason = 'refresh_token_reused';
  assert.equal(isCodexOverviewAccountAbnormal(value), false);
});
