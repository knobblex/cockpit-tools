import assert from 'node:assert/strict';
import test from 'node:test';
import { deferred, loadHookModule, settlePromises } from '../../../tests/helpers/reactHookHarness';
import type { CodexAccount } from '../../types/codex';
import type { CodexLocalAccessCollection } from '../../types/codexLocalAccess';
import type { PremiumSeatConfig, PremiumSeatState } from '../../services/codexPremiumSeatService';
import * as eligibility from '../../utils/codexPremiumSeatEligibility';

function nodes(value: any): any[] {
  if (!value || typeof value !== 'object') return [];
  if (Array.isArray(value)) return value.flatMap(nodes);
  return [value, ...nodes(value.props?.children)];
}
function text(value: any): string {
  if (value == null || typeof value === 'boolean') return '';
  if (Array.isArray(value)) return value.map(text).join('');
  return typeof value === 'object' ? text(value.props?.children) : String(value);
}
const workspace = '12345678-1234-4234-8234-123456789abc';
function account(id: string, patch: Partial<CodexAccount> = {}): CodexAccount {
  return { id, email: `${id}@example.com`, auth_mode: 'chatgpt', plan_type: 'business', account_id: workspace,
    user_id: `${id}-user`, tokens: { access_token: 'access', refresh_token: 'refresh', id_token: '' },
    created_at: 1, last_used: 1, ...patch };
}
function config(patch: Partial<PremiumSeatConfig> = {}): PremiumSeatConfig {
  return { enabled: false, workspaceId: workspace, adminAccountId: 'admin', apiKeyId: 'key', useApiKeyPool: true,
    memberAccountIds: [], probeModel: '', ...patch };
}
function state(patch: Partial<PremiumSeatState> = {}): PremiumSeatState {
  return { version: 1, revision: 0, config: null, paused: false, phase: 'idle', holderAccountId: null, holderUserId: null,
    validatedAt: null, operation: null, history: [], exhaustedUntil: {}, message: null, nextCheckAt: null, ...patch };
}
function collection(patch: Partial<CodexLocalAccessCollection> = {}): CodexLocalAccessCollection {
  return { enabled: true, gatewayMode: 'native', accountIds: ['admin', 'member'],
    apiKeys: [{ id: 'key', label: 'Client key', key: 'secret', enabled: true, inheritAccountPool: true,
      allowedModels: [], excludedModels: [], tokenUsed: 0, createdAt: 1, updatedAt: 1 }],
    ...patch } as CodexLocalAccessCollection;
}
function harness(initialAccounts = [account('admin'), account('member')], initialState = state(), initialCollection = collection(), options: { failInitialLoad?: boolean; delayInitialLoad?: boolean } = {}) {
  let accounts = initialAccounts;
  let remote = initialState;
  let currentCollection = initialCollection;
  let loadFailure = Boolean(options.failInitialLoad);
  let pendingLoad = options.delayInitialLoad ? deferred<PremiumSeatState>() : undefined;
  const timeouts = new Map<number, () => void>();
  let nextTimeout = 0;
  let poll: (() => Promise<void>) | undefined;
  let pendingControl: ReturnType<typeof deferred<PremiumSeatState>> | undefined;
  const calls = { get: 0, checked: [] as PremiumSeatConfig[], configured: [] as PremiumSeatConfig[], actions: [] as string[] };
  const hook = loadHookModule(new URL('./CodexPremiumSeatRotationControl.tsx', import.meta.url), {
    '../SingleSelectDropdown': { SingleSelectDropdown: 'Dropdown' },
    '../../utils/codexPremiumSeatEligibility': eligibility,
    '../../services/codexPremiumSeatService': {
      async getPremiumSeat() { calls.get++; if (loadFailure) throw new Error('Cannot read saved state'); return pendingLoad ? pendingLoad.promise : remote; },
      async checkPremiumSeat(value: PremiumSeatConfig) { calls.checked.push(value); return remote; },
      async configurePremiumSeat(value: PremiumSeatConfig) {
        calls.configured.push(value); remote = { ...remote, config: value }; return remote;
      },
      async controlPremiumSeat(action: string) {
        calls.actions.push(action);
        if (pendingControl) return pendingControl.promise;
        if (action === 'pause') remote = { ...remote, paused: true };
        return remote;
      },
    },
  }, { setInterval(callback: () => Promise<void>) { poll = callback; return 1; }, clearInterval() { poll = undefined; },
    setTimeout(callback: () => void) { const id = ++nextTimeout; timeouts.set(id, callback); return id; },
    clearTimeout(id: number) { timeouts.delete(id); } });
  hook.render(() => hook.exports.CodexPremiumSeatRotationControl({ accounts, collection: currentCollection }));
  const tree = () => nodes(hook.flush());
  const button = (label: string) => tree().find((node) => node.type === 'button' && text(node) === label)!;
  const dropdown = (label: string) => tree().find((node) => node.type === 'Dropdown' && node.props.ariaLabel === label)!;
  const toggle = () => tree().find((node) => node.props?.role === 'switch')!;
  return { hook, tree, button, dropdown, toggle, calls, visible: () => text(hook.flush()),
    setAccounts(next: CodexAccount[]) { accounts = next; }, setCollection(next: CodexLocalAccessCollection) { currentCollection = next; },
    setRemote(next: PremiumSeatState) { remote = next; }, failLoad() { loadFailure = true; },
    recoverLoad() { loadFailure = false; pendingLoad = undefined; },
    delayLoad() { pendingLoad = deferred<PremiumSeatState>(); return pendingLoad; },
    get delayedLoad() { return pendingLoad; },
    async expireRead() { const [id, callback] = [...timeouts.entries()][0]; timeouts.delete(id); callback(); await settlePromises(); },
    async poll() { await poll?.(); await settlePromises(); },
    delayControl() { pendingControl = deferred<PremiumSeatState>(); return pendingControl; } };
}

test('personal and unknown accounts have no new entry, while saved-state loading still runs', async () => {
  for (const patch of [{ plan_type: 'free' }, { plan_type: 'plus' }, { plan_type: 'pro' }, { plan_type: undefined },
    { plan_type: 'business', account_structure: 'personal' }]) {
    const h = harness([account('personal', patch)]);
    assert.equal(h.hook.flush(), null);
    await settlePromises();
    assert.equal(h.hook.flush(), null);
    assert.equal(h.calls.get, 1);
    assert.equal(h.calls.configured.length, 0);
    h.hook.unmount();
  }
});

test('teams remain disabled by default and save after explicit selection with an informational notice', async () => {
  const h = harness(); await settlePromises();
  assert.equal(h.toggle().props.checked, false);
  assert.equal(h.toggle().props.disabled, true);
  h.toggle().props.onChange({ target: { checked: true } });
  assert.equal(h.toggle().props.checked, false);
  h.dropdown('管理员授权').props.onChange('admin');
  h.dropdown('席位调换 API Key').props.onChange('key');
  assert.equal(h.toggle().props.disabled, false);
  assert.equal(h.toggle().props.checked, false);
  h.toggle().props.onChange({ target: { checked: true } });
  assert.match(h.visible(), /变更会影响来源和目标成员在工作区及其他客户端的权益/);
  assert.match(h.visible(), /初次启用和每次调换会消耗少量额度执行定向探测/);
  assert.equal(h.tree().some((node) => node.type === 'input' && node.props.type === 'checkbox' && !node.props.role), false);
  assert.equal(h.button('保存配置').props.disabled, false);
  h.button('保存配置').props.onClick(); await settlePromises();
  assert.equal(h.calls.configured.length, 1);
  assert.equal(h.calls.configured[0].enabled, true);
  assert.deepEqual(h.calls.actions, []);
  h.hook.unmount();
});

test('mixed pools keep only same-workspace local team candidates and never offer personal administrators', async () => {
  const members = [account('admin'), account('member', { plan_type: 'prolite', account_structure: 'workspace' }),
    account('personal', { plan_type: 'business', account_structure: 'personal' }), account('unknown', { plan_type: undefined }),
    account('other', { account_id: 'other-workspace' }), account('apikey', { auth_mode: 'apikey' }),
    account('web', { token_source_mode: 'chatgpt_web_session' }), account('pending', { authorization_status: 'pending' }),
    account('grant', { api_provider_id: 'cockpit_chatgpt_plan' }), account('agent', { agent_identity: {} as CodexAccount['agent_identity'] }),
    account('paused'), account('official')];
  const h = harness(members, state({ config: config() }), collection({ accountIds: members.map((a) => a.id),
    pausedAccountIds: ['paused'], officialAccountBindings: { official: 'grant' } }));
  await settlePromises();
  assert.deepEqual(Array.from(h.dropdown('管理员授权').props.options, (item: any) => item.value), ['admin', 'member', 'other', 'paused', 'official']);
  assert.deepEqual(h.tree().filter((node) => node.props?.className === 'premium-seat-member-email').map(text), ['admin@example.com', 'member@example.com']);
  h.dropdown('管理员授权').props.onChange('personal');
  assert.equal(h.dropdown('管理员授权').props.value, 'admin');
  h.hook.unmount();
});

test('team metadata only offers configuration and does not promise remote management capability', async () => {
  for (const plan_type of ['enterprise', 'edu_k12']) {
    const h = harness([account('admin', { plan_type }), account('member', { plan_type })]);
    await settlePromises();
    assert.match(h.visible(), /团队类型仅用于预筛/);
    assert.equal(h.toggle().props.checked, false);
    assert.deepEqual(h.calls.checked, []);
    assert.equal(h.calls.configured.length, 0);
    h.hook.unmount();
  }
});

test('removing the last team retains old settings and allows pause and disabling', async () => {
  const h = harness(undefined, state({ config: config({ enabled: true }), phase: 'ready' }));
  await settlePromises();
  h.setAccounts([]);
  assert.match(h.visible(), /已有配置和操作记录仍保留/);
  assert.equal(h.button('切换下一位').props.disabled, true);
  h.button('切换下一位').props.onClick();
  h.button('只读核对').props.onClick();
  h.button('保存配置').props.onClick();
  assert.deepEqual(h.calls.actions, []);
  assert.deepEqual(h.calls.checked, []);
  assert.equal(h.calls.configured.length, 0);
  assert.equal(h.button('暂停调换').props.disabled, false);
  h.button('暂停调换').props.onClick(); await settlePromises();
  assert.deepEqual(h.calls.actions, ['pause']);
  assert.equal(h.button('重新核对并恢复').props.disabled, true);
  h.button('重新核对并恢复').props.onClick();
  assert.deepEqual(h.calls.actions, ['pause']);
  assert.equal(h.toggle().props.disabled, false);
  h.toggle().props.onChange({ target: { checked: false } });
  assert.equal(h.button('保存配置').props.disabled, false);
  h.button('保存配置').props.onClick(); await settlePromises();
  assert.equal(h.calls.configured[0].enabled, false);
  assert.equal(h.toggle().props.disabled, true);
  assert.notEqual(h.hook.flush(), null, 'disabled historical configuration remains manageable');
  h.hook.unmount();
});

test('pending operations retain only reconciliation controls without a team and cannot overwrite settings', async () => {
  const h = harness([], state({ config: config({ enabled: true }), paused: true, phase: 'needs_attention',
    operation: { id: 'operation', workspaceId: workspace, sourceAccountId: 'admin', targetAccountId: 'member',
      sourceUserId: 'a', targetUserId: 'b', beforeCounts: { prolite: 1, default: 1 }, createdAt: 1,
      submittedAt: 2, exhaustionTriggered: true, probeStarted: false, verifiedAt: null, resolvedWithoutSwap: false } }));
  await settlePromises();
  assert.match(h.visible(), /操作 ID：operation/);
  for (const label of ['切换下一位', '保存配置', '只读核对']) {
    assert.equal(h.button(label).props.disabled, true); h.button(label).props.onClick();
  }
  assert.equal(h.toggle().props.disabled, true);
  h.toggle().props.onChange({ target: { checked: false } });
  assert.equal(h.toggle().props.checked, true);
  assert.equal(h.button('重新核对未决操作').props.disabled, false);
  h.button('重新核对未决操作').props.onClick(); await settlePromises();
  assert.equal(h.button('核对并确认未执行').props.disabled, false);
  h.button('核对并确认未执行').props.onClick(); await settlePromises();
  assert.deepEqual(h.calls.actions, ['resume', 'resolve_unapplied']);
  assert.equal(h.calls.configured.length, 0);
  assert.deepEqual(h.calls.checked, []);
  h.hook.unmount();
});

test('paused members and disabled keys block new attempts but never prevent switching the feature off', async () => {
  const initial = state({ config: config({ enabled: true }), phase: 'ready' });
  for (const currentCollection of [collection({ pausedAccountIds: ['member'] }),
    collection({ apiKeys: collection().apiKeys.map((key) => ({ ...key, enabled: false })) }),
    collection({ gatewayMode: 'sidecar' })]) {
    const h = harness(undefined, initial, currentCollection); await settlePromises();
    assert.equal(h.button('切换下一位').props.disabled, true);
    h.button('切换下一位').props.onClick();
    assert.deepEqual(h.calls.actions, []);
    h.toggle().props.onChange({ target: { checked: false } });
    assert.equal(h.toggle().props.checked, false);
    assert.equal(h.button('保存配置').props.disabled, false);
    h.button('保存配置').props.onClick(); await settlePromises();
    assert.equal(h.calls.configured[0].enabled, false);
    h.hook.unmount();
  }
});

test('model changes remain saveable while an ineligible workspace blocks checking and saving', async () => {
  const h = harness(undefined, state({ config: config({ enabled: true }) })); await settlePromises();
  assert.equal(h.button('保存配置').props.disabled, false);
  h.tree().find((node) => node.type === 'input' && node.props.placeholder === '自动发现，无需填写').props.onChange({ target: { value: 'new-model' } });
  assert.equal(h.button('保存配置').props.disabled, false);
  h.tree().find((node) => node.type === 'input' && node.props.placeholder === '选择管理员后自动填入').props.onChange({ target: { value: 'different-workspace' } });
  assert.equal(h.button('只读核对').props.disabled, true);
  assert.equal(h.button('保存配置').props.disabled, true);
  h.hook.unmount();
});

test('provider and model-routing keys are not offered or treated as ordinary OAuth pools', async () => {
  const base = collection().apiKeys[0];
  const apiKeys = [base, { ...base, id: 'provider', providerGateway: {} }, { ...base, id: 'routed', modelRouting: {} }];
  for (const apiKeyId of ['provider', 'routed']) {
    const h = harness(undefined, state({ config: config({ enabled: true, apiKeyId }), phase: 'ready' }), collection({ apiKeys }));
    await settlePromises();
    assert.deepEqual(Array.from(h.dropdown('席位调换 API Key').props.options, (item: any) => item.value), ['key']);
    assert.equal(h.button('切换下一位').props.disabled, true);
    assert.equal(h.button('只读核对').props.disabled, true);
    h.button('切换下一位').props.onClick();
    assert.deepEqual(h.calls.actions, []);
    assert.match(h.visible(), /请选择已启用的普通 OAuth 账号池 Key/);
    h.hook.unmount();
  }
});

test('valid teams preserve next-member behavior and overlapping clicks are deduplicated', async () => {
  const initial = state({ config: config({ enabled: true }), phase: 'ready' });
  const h = harness(undefined, initial); await settlePromises();
  assert.equal(h.button('切换下一位').props.disabled, false);
  const waiting = h.delayControl();
  const click = h.button('切换下一位').props.onClick;
  click(); click();
  assert.deepEqual(h.calls.actions, ['next']);
  assert.equal(h.button('切换下一位').props.disabled, true);
  waiting.resolve(initial); await settlePromises();
  assert.equal(h.button('切换下一位').props.disabled, false);
  h.hook.unmount();
});

test('a failed saved-state read remains visible even without team accounts', async () => {
  const h = harness([]); await settlePromises();
  h.failLoad(); await h.poll();
  assert.match(h.visible(), /Cannot read saved state/);
  h.hook.unmount();
});

test('initial read errors end loading, stop automatic retries, and recover only on manual reload', async () => {
  const h = harness(undefined, state(), undefined, { failInitialLoad: true });
  await settlePromises();
  assert.match(h.visible(), /读取失败/);
  assert.doesNotMatch(h.visible(), /加载中/);
  assert.match(h.visible(), /Cannot read saved state/);
  assert.equal(h.button('保存配置').props.disabled, true);
  assert.equal(h.toggle().props.disabled, true);
  await h.poll();
  assert.equal(h.calls.get, 1, 'a rejected state read must not be polled repeatedly');
  h.recoverLoad();
  h.button('重新加载').props.onClick(); h.hook.flush(); await settlePromises();
  assert.equal(h.calls.get, 2);
  assert.match(h.visible(), /未启用/);
  assert.doesNotMatch(h.visible(), /Cannot read saved state|读取失败|加载中/);
  assert.deepEqual(h.calls.checked, []);
  assert.deepEqual(h.calls.actions, []);
  h.hook.unmount();
});

test('an unanswered state read times out and its late response cannot overwrite a retry', async () => {
  const h = harness(undefined, state(), undefined, { delayInitialLoad: true });
  const late = h.delayedLoad!;
  assert.match(h.visible(), /加载中/);
  await h.expireRead();
  assert.match(h.visible(), /读取失败/);
  assert.match(h.visible(), /读取席位状态超时/);
  assert.equal(h.toggle().props.disabled, true);
  await h.poll();
  assert.equal(h.calls.get, 1);
  h.recoverLoad();
  h.button('重新加载').props.onClick(); h.hook.flush(); await settlePromises();
  assert.match(h.visible(), /未启用/);
  late.resolve(state({ config: config({ enabled: true }), phase: 'ready' }));
  await settlePromises();
  assert.equal(h.toggle().props.checked, false, 'the timed-out result cannot enable or replace the loaded configuration');
  assert.doesNotMatch(h.visible(), /读取失败|读取席位状态超时/);
  assert.deepEqual(h.calls.actions, []);
  h.hook.unmount();
});

test('local state refreshes do not overlap or trigger remote seat actions', async () => {
  const h = harness(); await settlePromises();
  const pending = h.delayLoad();
  await h.poll(); await h.poll();
  assert.equal(h.calls.get, 2, 'one initial read and one in-flight refresh');
  pending.resolve(state()); await settlePromises(); h.recoverLoad();
  await h.poll();
  assert.equal(h.calls.get, 3);
  assert.deepEqual(h.calls.checked, []);
  assert.deepEqual(h.calls.configured, []);
  assert.deepEqual(h.calls.actions, []);
  h.hook.unmount();
});

test('successful local reads do not erase manual operation failures', async () => {
  const h = harness(undefined, state({ config: config({ enabled: true }), phase: 'ready' }));
  await settlePromises();
  const pending = h.delayControl();
  h.button('切换下一位').props.onClick();
  pending.reject(new Error('Seat operation failed'));
  await settlePromises();
  assert.match(h.visible(), /Seat operation failed/);
  await h.poll();
  assert.match(h.visible(), /Seat operation failed/);
  assert.deepEqual(h.calls.actions, ['next']);
  h.hook.unmount();
});
