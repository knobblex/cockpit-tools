import assert from 'node:assert/strict';
import test from 'node:test';
import { loadHookModule, settlePromises } from '../../../tests/helpers/reactHookHarness.ts';

function setup(managed = false, state = managed ? 'running' : 'stopped') {
  const calls: { action: string; config: unknown }[] = [];
  const config = { backend_path: '/bin/backend', state_dir: '/private/state', port: 8317, model: 'gemini', auto_start: true };
  let timer: (() => void) | null = null;
  const h = loadHookModule(new URL('./AntigravityUpstreamTool.tsx', import.meta.url), {
    '@tauri-apps/api/core': { invoke: async (command: string, args: { action: string; config: unknown }) => {
      assert.equal(command, 'local_tools_antigravity');
      calls.push(args);
      return { config, managed, state, pid: null, last_exit_code: null, healthy: false, models: [], detail: '', log_path: '/log', base_url: 'http://127.0.0.1:8317/v1' };
    } },
    '@tauri-apps/plugin-dialog': { open: async () => null },
    'react-i18next': { useTranslation: () => ({ i18n: { language: 'zh-CN' } }) },
  }, { window: { setInterval: (callback: () => void) => { timer = callback; return 1; }, clearInterval: () => { timer = null; } } });
  const render = () => h.render(() => h.exports.AntigravityUpstreamTool());
  const all = (node: any): any[] => !node || typeof node !== 'object' ? [] : Array.isArray(node)
    ? node.flatMap(all) : [node, ...all(node.props?.children)];
  const find = (label: string) => all(render()).find(node => node.type === 'button' && node.props.children === label);
  return { h, calls, render, all, find, tick: () => timer?.(), hasTimer: () => timer !== null };
}

test('mount only checks status; unmount stops polling', async () => {
  const x = setup();
  x.render(); await settlePromises();
  assert.deepEqual(x.calls.map(call => call.action), ['status']);
  assert.equal(x.find('启动').props.disabled, false);
  x.h.unmount(); assert.equal(x.hasTimer(), false);
});

test('external service cannot be started or stopped by the UI', async () => {
  const x = setup(false, 'external');
  x.render(); await settlePromises();
  assert.equal(x.find('启动').props.disabled, true);
  assert.equal(x.find('停止').props.disabled, true);
  x.h.unmount();
});

test('stop requires explicit confirmation; managed configuration is disabled', async () => {
  const x = setup(true);
  x.render(); await settlePromises();
  assert.equal(x.all(x.render()).find(node => node.type === 'fieldset').props.disabled, true);
  x.find('停止').props.onClick();
  assert.deepEqual(x.calls.map(call => call.action), ['status']);
  x.tick(); await settlePromises();
  assert.ok(x.find('确认停止'), 'status polling must not dismiss a confirmation');
  x.find('确认停止').props.onClick(); await settlePromises();
  assert.deepEqual(x.calls.map(call => call.action), ['status', 'status', 'stop']);
  x.h.unmount();
});

test('unsaved form changes survive status refresh and block start', async () => {
  const x = setup();
  x.render(); await settlePromises();
  const input = x.all(x.render()).find(node => node.type === 'input' && node.props.value === '/bin/backend');
  input.props.onChange({ target: { value: '/other/cliproxyapi' } });
  x.tick(); await settlePromises();
  assert.ok(x.all(x.render()).some(node => node.type === 'input' && node.props.value === '/other/cliproxyapi'));
  assert.equal(x.find('启动').props.disabled, true);
  assert.equal(x.find('保存配置').props.disabled, false);
  x.h.unmount();
});

test('autostart is an explicit saved setting, with no Python input', async () => {
  const x = setup();
  x.render(); await settlePromises();
  const inputs = x.all(x.render()).filter(node => node.type === 'input');
  assert.equal(inputs.length, 5);
  const checkbox = inputs.find(node => node.props.type === 'checkbox');
  assert.equal(checkbox.props.checked, true);
  checkbox.props.onChange({ target: { checked: false } });
  x.find('保存配置').props.onClick(); await settlePromises();
  const saved = x.calls[x.calls.length - 1].config as { auto_start: boolean };
  assert.equal(saved.auto_start, false);
  assert.equal('python_path' in saved, false);
  x.h.unmount();
});
