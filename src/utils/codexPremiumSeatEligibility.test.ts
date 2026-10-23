import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import type { CodexAccount } from '../types/codex';
import { hasPremiumSeatTeamIdentity, isPremiumSeatTeamAccount } from './codexPremiumSeatEligibility';

const cases: { name: string; planType: string | null; accountStructure: string | null; eligible: boolean }[] = JSON.parse(
  readFileSync(new URL('../../tests/codex-seat-rotation/team-eligibility-cases.json', import.meta.url), 'utf8'),
);
for (const item of cases) {
  test(`team identity agrees with Rust: ${item.name}`, () => {
    assert.equal(hasPremiumSeatTeamIdentity(item.planType, item.accountStructure), item.eligible);
  });
}

const account: CodexAccount = {
  id: 'member', email: 'member@example.com', auth_mode: 'chatgpt', plan_type: 'business',
  account_id: 'workspace', user_id: 'member-user', tokens: { access_token: 'access', refresh_token: 'refresh', id_token: '' },
  created_at: 1, last_used: 1,
};
test('only identifiable local OAuth team members pass the UI prefilter', () => {
  assert.equal(isPremiumSeatTeamAccount(account), true);
  const excluded: Partial<CodexAccount>[] = [
    { auth_mode: ' APIKEY ' },
    { agent_identity: { agent_runtime_id: 'agent', agent_private_key: '', account_id: 'workspace', chatgpt_user_id: 'member-user' } },
    { api_provider_id: 'cockpit_chatgpt_plan' },
    { token_source_mode: 'chatgpt_web_session' },
    { authorization_status: ' PENDING ' },
    { tokens: { access_token: '', refresh_token: '', id_token: '' }, account_note: 'not yet authorized' },
    { account_id: undefined }, { account_id: '  ' }, { user_id: undefined }, { user_id: '  ' },
    { account_structure: 'personal' }, { plan_type: 'pro' },
  ];
  for (const patch of excluded) assert.equal(isPremiumSeatTeamAccount({ ...account, ...patch }), false, JSON.stringify(patch));
  assert.equal(isPremiumSeatTeamAccount({ ...account, plan_type: 'prolite', account_structure: 'workspace' }), true);
});
