import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Route } from 'lucide-react';
import { SingleSelectDropdown } from '../SingleSelectDropdown';
import { updateCodexLocalAccessForwardingPolicy } from '../../services/codexLocalAccessService';
import type { CodexAccount } from '../../types/codex';
import type { CodexForwardingMode, CodexForwardingPolicy, CodexLocalAccessCollection, CodexLocalAccessState } from '../../types/codexLocalAccess';
import './CodexForwardingPolicyControl.css';

export function CodexForwardingPolicyControl({ accounts, collection, disabled = false, onSaved }: {
  accounts: CodexAccount[];
  collection: CodexLocalAccessCollection | null | undefined;
  disabled?: boolean;
  onSaved: (state: CodexLocalAccessState) => void;
}) {
  const { t } = useTranslation();
  const saved = JSON.stringify(collection?.forwardingPolicy ?? { mode: 'all', accountIds: [] });
  const [draft, setDraft] = useState<CodexForwardingPolicy>(() => JSON.parse(saved));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const dirty = useRef(false);
  const pending = useRef(false);
  useEffect(() => {
    if (!dirty.current) setDraft(JSON.parse(saved));
  }, [saved]);
  const ids = new Set([...(collection?.accountIds ?? []), ...(collection?.imageGenerationAccountIds ?? [])]);
  for (const key of collection?.apiKeys ?? []) {
    for (const id of key.accountIds ?? []) ids.add(id);
    const routing = key.modelRouting as { routes?: { providerAccountId?: string }[] } | null | undefined;
    for (const route of routing?.routes ?? []) {
      if (route.providerAccountId) ids.add(route.providerAccountId);
    }
  }
  const candidates = accounts.filter((account) => ids.has(account.id));
  const unavailableIds = draft.accountIds.filter((id) => !candidates.some((account) => account.id === id));
  const blocked = disabled || saving || !collection;
  const change = (next: CodexForwardingPolicy) => {
    dirty.current = true;
    setDraft(next);
    setError('');
  };
  const save = async () => {
    if (blocked || pending.current || (draft.mode === 'selected_accounts' && draft.accountIds.length === 0)) return;
    pending.current = true;
    setSaving(true);
    setError('');
    try {
      const next = await updateCodexLocalAccessForwardingPolicy(draft);
      dirty.current = false;
      setDraft(next.collection?.forwardingPolicy ?? { mode: 'all', accountIds: [] });
      onSaved(next);
    } catch (e) {
      setError(String(e));
    } finally {
      pending.current = false;
      setSaving(false);
    }
  };
  return <section className="codex-api-service-panel codex-forwarding-policy" aria-busy={saving}>
    <div className="codex-api-service-panel-head">
      <h2 className="codex-api-service-title-with-icon"><Route size={16} />{t('codex.apiService.forwarding.title', '转发策略')}</h2>
      <button type="button" className="btn btn-secondary btn-sm" onClick={() => void save()}
        disabled={blocked || (draft.mode === 'selected_accounts' && draft.accountIds.length === 0)}>
        {t('common.save', '保存')}
      </button>
    </div>
    <SingleSelectDropdown value={draft.mode} options={[
      { value: 'all', label: t('codex.apiService.forwarding.all', '全部可用账号') },
      { value: 'premium_only', label: t('codex.apiService.forwarding.premiumOnly', '只转发高级席位') },
      { value: 'selected_accounts', label: t('codex.apiService.forwarding.selectedAccounts', '指定账号') },
    ]} onChange={(value) => change({ ...draft, mode: value as CodexForwardingMode })}
      disabled={blocked} ariaLabel={t('codex.apiService.forwarding.title', '转发策略')} />
    <p className="codex-forwarding-hint">{t('codex.apiService.forwarding.hint', '适用于对话和生图请求；继续遵守 Key 范围、模型权限与额度保护。模式切换后点击保存生效。')}</p>
    {draft.mode === 'premium_only' && <p className="codex-forwarding-hint">{t('codex.apiService.forwarding.premiumHint', '仅使用已确认团队身份且套餐为 prolite 的本地 OAuth 高级席位。资料未知或没有可用高级席位时停止转发，请先刷新账号资料。')}</p>}
    {draft.mode === 'selected_accounts' && <fieldset disabled={blocked} className="codex-forwarding-accounts">
      <legend>{t('codex.apiService.forwarding.selectedAccounts', '指定账号')}</legend>
      {[...candidates.map((account) => ({ id: account.id, label: account.email })),
        ...unavailableIds.map((id) => ({ id, label: `${id} (${t('codex.apiService.forwarding.unavailable', '不可用，请取消选择')})` }))].map(({ id, label }) => <label key={id}>
        <input type="checkbox" checked={draft.accountIds.includes(id)} onChange={(event) => change({ ...draft,
          accountIds: event.target.checked ? [...draft.accountIds, id] : draft.accountIds.filter((item) => item !== id),
        })} /><span>{label}</span>
      </label>)}
      {draft.accountIds.length === 0 && <p className="codex-forwarding-hint">{t('codex.apiService.forwarding.selectRequired', '请至少选择一个账号。')}</p>}
    </fieldset>}
    {error && <p role="alert" className="codex-forwarding-error">{error}</p>}
  </section>;
}
