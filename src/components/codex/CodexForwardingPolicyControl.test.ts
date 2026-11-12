import assert from 'node:assert/strict';
import test from 'node:test';
import { deferred, loadHookModule, settlePromises } from '../../../tests/helpers/reactHookHarness';
import type { CodexForwardingPolicy } from '../../types/codexLocalAccess';

function nodes(value: any): any[] {
  if (!value || typeof value !== 'object') return [];
  if (Array.isArray(value)) return value.flatMap(nodes);
  return [value, ...nodes(value.props?.children)];
}
function harness(policy?: CodexForwardingPolicy) {
  let collection: any = { accountIds: ['a', 'b'], imageGenerationAccountIds: ['image'], forwardingPolicy: policy };
  const response = deferred<any>();
  const calls: CodexForwardingPolicy[] = [];
  const h = loadHookModule(new URL('./CodexForwardingPolicyControl.tsx', import.meta.url), {
    'react-i18next': { useTranslation: () => ({ t: (_key: string, fallback: string) => fallback }) },
    '../SingleSelectDropdown': { SingleSelectDropdown: 'Dropdown' },
    '../../services/codexLocalAccessService': {
      updateCodexLocalAccessForwardingPolicy: (value: CodexForwardingPolicy) => {
        calls.push(value); return response.promise;
      },
    },
  });
  let saved = 0;
  const render = () => nodes(h.render(() => h.exports.CodexForwardingPolicyControl({
    accounts: ['a', 'b', 'image', 'outside'].map((id) => ({ id, email: `${id}@example.com` })),
    collection, onSaved(next: any) { saved++; collection = next.collection; },
  })));
  return { render, calls, response, saved: () => saved,
    keys(next: any[]) { collection = { ...collection, apiKeys: next }; return render(); },
    refresh(next: any) { collection = { ...collection, forwardingPolicy: next }; return render(); },
    mode(value: string) { render().find((node) => node.type === 'Dropdown').props.onChange(value); return render(); },
    save() { render().find((node) => node.type === 'button').props.onClick(); return render(); },
  };
}

test('legacy configurations keep all accounts until a mode is explicitly saved', async () => {
  const h = harness();
  assert.equal(h.render().find((node) => node.type === 'Dropdown').props.value, 'all');
  h.mode('premium_only');
  assert.equal(h.calls.length, 0);
  h.save(); h.save();
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].mode, 'premium_only');
  assert.equal(h.render().find((node) => node.type === 'Dropdown').props.disabled, true);
  h.response.resolve({ collection: { accountIds: ['a', 'b'], imageGenerationAccountIds: ['image'], forwardingPolicy: h.calls[0] } });
  await settlePromises();
  assert.equal(h.saved(), 1);
});

test('selected mode requires a selection and saves multiple chat/image accounts', async () => {
  const h = harness();
  h.mode('selected_accounts');
  assert.equal(h.render().find((node) => node.type === 'button').props.disabled, true);
  h.save();
  assert.equal(h.calls.length, 0);
  const inputs = () => h.render().filter((node) => node.type === 'input');
  assert.equal(inputs().length, 3);
  inputs()[0].props.onChange({ target: { checked: true } });
  inputs()[2].props.onChange({ target: { checked: true } });
  h.save();
  assert.deepEqual(Array.from(h.calls[0].accountIds), ['a', 'image']);
  h.response.resolve({ collection: { accountIds: ['a', 'b'], imageGenerationAccountIds: ['image'], forwardingPolicy: h.calls[0] } });
  await settlePromises();
  h.mode('premium_only'); h.mode('selected_accounts');
  assert.equal(inputs()[0].props.checked, true);
  assert.equal(inputs()[2].props.checked, true);
});

test('refreshes preserve unsaved choices and failed saves retain the draft', async () => {
  const h = harness({ mode: 'all', accountIds: [] });
  h.mode('selected_accounts');
  h.render().find((node) => node.type === 'input').props.onChange({ target: { checked: true } });
  h.refresh({ mode: 'premium_only', accountIds: [] });
  assert.equal(h.render().find((node) => node.type === 'Dropdown').props.value, 'selected_accounts');
  h.save();
  h.response.reject(new Error('Disk write failed'));
  await settlePromises();
  assert.equal(h.saved(), 0);
  assert.match(h.render().find((node) => node.props?.role === 'alert').props.children, /Disk write failed/);
  assert.equal(h.render().find((node) => node.type === 'input').props.checked, true);
});

test('saved modes hydrate on refresh and removed accounts can be deselected', () => {
  const h = harness();
  h.refresh({ mode: 'selected_accounts', accountIds: ['removed'] });
  assert.equal(h.render().find((node) => node.type === 'Dropdown').props.value, 'selected_accounts');
  const inputs = h.render().filter((node) => node.type === 'input');
  const missing = inputs[inputs.length - 1];
  assert.equal(missing.props.checked, true);
  missing.props.onChange({ target: { checked: false } });
  assert.equal(h.render().find((node) => node.type === 'button').props.disabled, true);
});

test('accounts granted only by a fixed Key scope or provider route remain selectable', () => {
  const h = harness();
  h.mode('selected_accounts');
  assert.equal(h.render().filter((node) => node.type === 'input').length, 3);
  h.keys([{ accountIds: ['outside'] }]);
  assert.equal(h.render().filter((node) => node.type === 'input').length, 4);
  h.keys([{ modelRouting: { routes: [{ providerAccountId: 'outside' }] } }]);
  assert.equal(h.render().filter((node) => node.type === 'input').length, 4);
});
