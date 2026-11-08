import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useTranslation } from 'react-i18next';
import './tool.css';

interface Config {
  backend_path: string;
  state_dir: string;
  port: number;
  model: string;
  auto_start: boolean;
}
interface Status {
  config: Config;
  managed: boolean;
  pid: number | null;
  last_exit_code: number | null;
  state: 'unconfigured' | 'stopped' | 'external' | 'starting' | 'running';
  healthy: boolean;
  models: string[];
  detail: string;
  log_path: string;
  base_url: string;
}
type Action = 'status' | 'save' | 'start' | 'stop';

export function AntigravityUpstreamTool() {
  const { i18n } = useTranslation();
  const zh = i18n.language.startsWith('zh');
  const text = (cn: string, en: string) => zh ? cn : en;
  const [status, setStatus] = useState<Status | null>(null);
  const [draft, setDraft] = useState<Config | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [confirmStop, setConfirmStop] = useState(false);
  const operation = useRef(false);
  const mounted = useRef(false);

  const run = useCallback(async (action: Action, config?: Config) => {
    if (operation.current) return;
    operation.current = true;
    setBusy(true);
    setError('');
    try {
      const result = await invoke<Status>('local_tools_antigravity', { action, config: config ?? null });
      if (!mounted.current) return;
      setStatus(result);
      setDraft((current) => action === 'save' || current === null ? result.config : current);
      if (action !== 'status') setConfirmStop(false);
    } catch (cause) {
      if (mounted.current) setError(String(cause));
    } finally {
      operation.current = false;
      if (mounted.current) setBusy(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void run('status');
    const timer = window.setInterval(() => void run('status'), 10_000);
    return () => { mounted.current = false; window.clearInterval(timer); };
  }, [run]);

  const dirty = !!draft && JSON.stringify(draft) !== JSON.stringify(status?.config);
  const labels = {
    unconfigured: text('未配置', 'Not configured'),
    stopped: text('已停止', 'Stopped'),
    external: text('外部服务（未托管）', 'External service (not managed)'),
    starting: text('等待就绪 / 异常重试中', 'Starting / retrying after failure'),
    running: text('运行中', 'Running'),
  };
  const fields: [keyof Omit<Config, 'port' | 'auto_start'>, string][] = [
    ['backend_path', text('CLIProxyAPI 可执行文件', 'CLIProxyAPI executable')],
    ['state_dir', text('原账号状态目录', 'Existing account state directory')],
    ['model', text('默认模型 ID', 'Default model ID')],
  ];

  async function chooseBackend() {
    try {
      const path = await open({ multiple: false });
      if (path && mounted.current) setDraft((current) => current ? { ...current, backend_path: path } : current);
    } catch (cause) {
      if (mounted.current) setError(String(cause));
    }
  }

  return (
    <section className="antigravity-upstream-tool" aria-busy={busy}>
      <div className="group-title">{text('本地工具 · Antigravity 上游服务', 'Local tools · Antigravity upstream')}</div>
      <div className="settings-group">
        <p>{text(
          'Rust 内置转发，不再依赖 Python。CLIProxyAPI 作为子进程运行；完全退出 Cockpit 时停止，关闭窗口但驻留托盘时继续运行。',
          'Built-in Rust relay, no Python required. CLIProxyAPI runs as a child and stops when Cockpit fully exits; closing to the tray keeps it running.',
        )}</p>
        <div className="upstream-status" role="status">
          <strong>{status ? labels[status.state] : error ? text('状态读取失败', 'Status unavailable') : text('正在读取状态…', 'Loading status…')}</strong>
          {status && <code>{status.base_url}</code>}
          {status?.pid && <span>CLIProxyAPI PID {status.pid}</span>}
          {status?.last_exit_code != null && status.last_exit_code !== 0 && (
            <span>{text('最近退出码', 'Last exit code')}: {status.last_exit_code}</span>
          )}
        </div>
        {status?.state === 'external' && <p className="upstream-warning">{text(
          '端口由外部进程占用。本工具不会结束它；迁移时请先卸载旧 launchd 上游服务或停止原进程。',
          'Another process owns this port. This tool will not stop it; unload the retired launchd service or stop the original process before migration.',
        )}</p>}
        {status?.detail && <p className="upstream-warning">{status.detail}</p>}
        {error && <p className="upstream-warning" role="alert">{error}</p>}
        {draft && <fieldset disabled={busy || !!status?.managed}>
          {fields.map(([key, label]) => <label className="upstream-field" key={key}>
            <span>{label}</span>
            <input className="settings-input" value={draft[key]} spellCheck={false}
              onChange={(event) => setDraft({ ...draft, [key]: event.target.value })} />
          </label>)}
          <div className="upstream-actions">
            <button className="btn btn-secondary" onClick={() => void chooseBackend()}>{text('选择 CLIProxyAPI…', 'Choose CLIProxyAPI…')}</button>
            <label className="upstream-port">{text('端口', 'Port')}
              <input className="settings-input" type="number" min={1024} max={65535} value={draft.port}
                onChange={(event) => setDraft({ ...draft, port: Number(event.target.value) })} />
            </label>
          </div>
          <label className="upstream-actions">
            <input type="checkbox" checked={draft.auto_start}
              onChange={(event) => setDraft({ ...draft, auto_start: event.target.checked })} />
            {text('随 Cockpit 启动上游服务', 'Start upstream with Cockpit')}
          </label>
        </fieldset>}
        <div className="upstream-actions">
          <button className="btn btn-secondary" disabled={busy || !draft || !dirty || !!status?.managed}
            onClick={() => draft && void run('save', draft)}>{text('保存配置', 'Save configuration')}</button>
          <button className="btn btn-primary" disabled={busy || dirty || !status?.config.backend_path || !!status?.managed || status?.state === 'external'}
            onClick={() => void run('start')}>{text('启动', 'Start')}</button>
          <button className="btn btn-secondary" disabled={busy || !status?.managed}
            onClick={() => setConfirmStop(true)}>{text('停止', 'Stop')}</button>
          <button className="btn btn-secondary" disabled={busy} onClick={() => void run('status')}>{text('刷新状态', 'Refresh status')}</button>
        </div>
        {confirmStop && <div className="upstream-confirm" role="alert">
          <p>{text('停止会中断正在进行的模型请求，确认停止？', 'Stopping interrupts active model requests. Continue?')}</p>
          <button className="btn btn-secondary" disabled={busy} onClick={() => void run('stop')}>{text('确认停止', 'Confirm stop')}</button>
          <button className="btn btn-secondary" disabled={busy} onClick={() => setConfirmStop(false)}>{text('取消', 'Cancel')}</button>
        </div>}
        {status?.healthy && <p>{text('鉴权模型列表正常（不代表额度充足）', 'Authenticated model list is healthy (quota is not verified)')}: {status.models.join(', ')}</p>}
        {status && <p>{text('服务日志', 'Service log')}: <code>{status.log_path}</code></p>}
        <p>{text('复用已有账号与密钥，不修改模型路由，不记录请求正文。修改配置前请先停止托管服务。',
          'Reuses existing accounts and keys; does not change model routing or capture request bodies. Stop the managed service before editing configuration.')}</p>
      </div>
    </section>
  );
}
