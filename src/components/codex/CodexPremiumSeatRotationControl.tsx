import { useEffect, useId, useRef, useState } from "react";
import { ArrowRight, ChevronDown, CircleAlert, Clock3, Pause, RefreshCw, Save, Settings2, ShieldCheck, SkipForward, Users } from "lucide-react";
import { SingleSelectDropdown } from "../SingleSelectDropdown";
import type { CodexAccount } from "../../types/codex";
import type { CodexLocalAccessApiKey, CodexLocalAccessCollection } from "../../types/codexLocalAccess";
import { isPremiumSeatTeamAccount } from "../../utils/codexPremiumSeatEligibility";
import { checkPremiumSeat, configurePremiumSeat, controlPremiumSeat, getPremiumSeat,
  type PremiumSeatConfig, type PremiumSeatState } from "../../services/codexPremiumSeatService";
import "./CodexPremiumSeatRotationControl.css";

const empty: PremiumSeatConfig = { enabled: false, workspaceId: "", adminAccountId: "", apiKeyId: "", useApiKeyPool: true, memberAccountIds: [], probeModel: "" };
const poolDraft = (config: PremiumSeatConfig | null) => ({ ...(config ?? empty), useApiKeyPool: true, memberAccountIds: [] });
const ordinaryKey = (key: CodexLocalAccessApiKey) => key.enabled && key.providerGateway == null && key.modelRouting == null;
const phases: Record<PremiumSeatState["phase"], string> = {
  idle: "等待核对 / 等待候选", ready: "已验证，按请求切换", draining: "等待在途请求结束",
  submitted: "已提交，正在核对", verifying: "验证目标成员", needs_attention: "需要处理",
};

export function CodexPremiumSeatRotationControl({ accounts, collection }: {
  accounts: CodexAccount[]; collection: CodexLocalAccessCollection | null | undefined;
}) {
  const [state, setState] = useState<PremiumSeatState | null>(null);
  const [draft, setDraft] = useState<PremiumSeatConfig>(empty);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [expanded, setExpanded] = useState(false);
  const formId = useId();
  const pending = useRef(false);
  const dirty = useRef(false);
  useEffect(() => {
    let alive = true;
    const refresh = async () => {
      try {
        const next = await getPremiumSeat();
        if (!alive || pending.current) return;
        setState(next);
        if (!dirty.current) setDraft(poolDraft(next.config));
        setLoaded(true);
      } catch (e) { if (alive) setError(String(e)); }
    };
    void refresh();
    const timer = setInterval(() => void refresh(), 5000);
    return () => { alive = false; clearInterval(timer); };
  }, []);
  const teamAccounts = accounts.filter(isPremiumSeatTeamAccount);
  const poolMembers = (config: PremiumSeatConfig) => {
    const key = collection?.apiKeys.find((k) => k.id === config.apiKeyId && ordinaryKey(k));
    const inherits = key?.inheritAccountPool ?? !(key?.accountIds?.length);
    const pool = key ? (inherits ? collection?.accountIds : key.accountIds) ?? [] : [];
    return pool.map((id) => teamAccounts.find((a) => a.id === id))
      .filter((a): a is CodexAccount => Boolean(a && a.account_id === config.workspaceId
        && !collection?.pausedAccountIds?.includes(a.id) && !collection?.officialAccountBindings?.[a.id]));
  };
  const candidates = poolMembers(draft);
  const eligibilityReason = (config: PremiumSeatConfig) => {
    if (!teamAccounts.some((a) => a.id === config.adminAccountId && a.account_id === config.workspaceId)) {
      return "仅团队账号可启用自动调换，请选择目标团队工作区的 OAuth 管理员授权。";
    }
    const key = collection?.apiKeys.find((k) => k.id === config.apiKeyId && ordinaryKey(k));
    if (!key) return "请选择已启用的普通 OAuth 账号池 Key。";
    if (poolMembers(config).length < 2) return "当前 Key 的有效池内至少需要两个同工作区的团队 OAuth 成员。";
    return "";
  };
  const draftEligibilityReason = eligibilityReason(draft);
  const savedEligibilityReason = state?.config ? eligibilityReason(state.config) : "请先保存席位调换配置";
  const display = (id: string | null | undefined) => accounts.find((a) => a.id === id)?.email ?? id ?? "—";
  const native = collection?.gatewayMode === "native" && collection.enabled;
  const unresolved = Boolean(state?.operation && !state.operation.verifiedAt && !state.operation.resolvedWithoutSwap);
  const edit = (next: Partial<PremiumSeatConfig>) => {
    if (pending.current || !loaded || unresolved) return;
    dirty.current = true; setDraft((old) => ({ ...old, ...next })); setNotice("");
  };
  const run = async (work: () => Promise<PremiumSeatState>, saved = false) => {
    if (pending.current) return;
    pending.current = true; setBusy(true); setError(""); setNotice("");
    try {
      const next = await work();
      if (saved) { dirty.current = false; setDraft(poolDraft(next.config)); }
      setState(next); setNotice(next.message ?? "配置已保存。");
    } catch (e) { setError(String(e)); }
    finally { pending.current = false; setBusy(false); }
  };
  const formDisabled = busy || !loaded || unresolved;
  const checkDisabled = formDisabled || !native || Boolean(draftEligibilityReason);
  const saveDisabled = formDisabled || (draft.enabled && (!native || Boolean(draftEligibilityReason)));
  const enableDisabled = formDisabled || (!draft.enabled && (!native || Boolean(draftEligibilityReason)));
  const enabled = Boolean(state?.config?.enabled);
  const statusTone = state?.phase === "needs_attention" ? "error" : enabled && !state?.paused ? "success" : "muted";
  const statusLabel = !loaded ? "加载中" : state?.paused ? "已暂停" : enabled ? phases[state!.phase] : "未启用";
  const savedKey = collection?.apiKeys.find((k) => k.id === state?.config?.apiKeyId);
  const nextDisabledReason = busy ? "正在处理席位操作" : !loaded ? "正在加载配置"
    : dirty.current ? "请先保存配置修改" : !native ? "请先启用原生 Rust API 网关"
    : !enabled ? "请先保存并启用席位调换配置" : unresolved ? "请先核对未完成的席位操作"
    : savedEligibilityReason || (state?.paused ? "请先重新核对并恢复" : state?.phase !== "ready" ? "等待当前高级成员验证通过" : "");
  const pauseDisabled = busy || !loaded || (!enabled && !unresolved) || Boolean(state?.paused);
  const resumeDisabled = busy || !loaded || !state?.paused || (!unresolved && (!enabled || Boolean(savedEligibilityReason)));
  // Loading saved state still runs above so removing the last team cannot hide recovery.
  if (!teamAccounts.length && !state?.config && !unresolved && !error) return null;
  return <section className="codex-api-service-panel premium-seat-control" aria-busy={busy}>
    <div className="premium-seat-header">
      <div className="premium-seat-heading">
        <h2>高级席位自动调换</h2>
        <span className={`codex-api-service-pill ${statusTone}`}>{statusLabel}</span>
      </div>
      <div className="premium-seat-actions">
      <button type="button" className="btn btn-secondary btn-sm" disabled={Boolean(nextDisabledReason)}
        title={nextDisabledReason || "无需等待额度耗尽，按账号池顺序调换到下一位可用成员；验证通过后更新路由"}
        onClick={() => { if (!nextDisabledReason) void run(() => controlPremiumSeat("next")); }}><SkipForward size={14} />切换下一位</button>
      <button type="button" className="btn btn-secondary btn-sm" aria-expanded={expanded} aria-controls={formId}
        onClick={() => setExpanded((value) => !value)}>
        <Settings2 size={14} />{expanded ? "收起配置" : "配置"}
        <ChevronDown size={13} className={`premium-seat-chevron${expanded ? " expanded" : ""}`} />
      </button>
      </div>
    </div>
    <p className="premium-seat-description">仅团队工作区可手动启用。正常调用沿用当前成员；实际请求明确返回额度耗尽时，按需核对并尝试下一位，验证通过后接入路由。</p>
    <dl className="premium-seat-overview">
      <div><dt><Users size={13} />当前高级成员</dt><dd>{display(state?.holderAccountId)}</dd></div>
      <div><dt><ShieldCheck size={13} />关联账号池</dt><dd>{savedKey?.label ?? "尚未配置"}</dd></div>
      <div><dt><Clock3 size={13} />{state?.validatedAt && !unresolved ? "下次额度检查" : "下次核对"}</dt><dd>{state?.nextCheckAt ? new Date(state.nextCheckAt * 1000).toLocaleString() : "—"}</dd></div>
    </dl>
    {!native && <div className="premium-seat-callout" role="status"><CircleAlert size={14} /><span>启用原生 Rust API 网关后，可核对并启用自动调换。</span></div>}
    {state?.config && savedEligibilityReason && <div className="premium-seat-callout" role="status"><CircleAlert size={14} /><span>{savedEligibilityReason}已有配置和操作记录仍保留，可暂停或核对；未决操作结束后可关闭。</span></div>}
    <div id={formId} hidden={!expanded} className="premium-seat-configuration">
      <div className="premium-seat-section-heading"><h3>工作区与账号池</h3>{dirty.current && <span className="codex-api-service-pill muted">未保存</span>}</div>
      {state?.config && !state.config.useApiKeyPool && <div className="premium-seat-callout"><CircleAlert size={14} /><span>当前使用旧版候选名单，保存后将跟随此 Key 的有效账号池。</span></div>}
      <fieldset disabled={formDisabled}>
        <div className="premium-seat-form-layout">
          <div className="premium-seat-fields">
            <div className="premium-seat-field"><span id={`${formId}-admin`}>管理员授权</span>
              <SingleSelectDropdown value={draft.adminAccountId} ariaLabel="管理员授权" disabled={formDisabled}
                placeholder="选择团队 Owner / Admin" options={teamAccounts.map((a) => ({ value: a.id, label: a.email }))}
                onChange={(value) => {
                  const a = teamAccounts.find((a) => a.id === value);
                  if (a) edit({ adminAccountId: value, workspaceId: a.account_id ?? "", memberAccountIds: [] });
                }} />
              <small>团队类型仅用于预筛；后台仍需核对真实工作区、Owner/Admin 权限和可调换席位。</small>
            </div>
            <label className="premium-seat-field"><span>工作区 ID</span><input value={draft.workspaceId} placeholder="选择管理员后自动填入"
              onChange={(e) => edit({ workspaceId: e.target.value.trim(), memberAccountIds: [] })} /></label>
            <div className="premium-seat-field"><span>API Key</span>
              <SingleSelectDropdown value={draft.apiKeyId} ariaLabel="席位调换 API Key" disabled={formDisabled}
                placeholder="选择关联的客户端 Key" options={(collection?.apiKeys ?? []).filter(ordinaryKey).map((k) => ({ value: k.id, label: k.label }))}
                onChange={(value) => edit({ apiKeyId: value, memberAccountIds: [] })} />
            </div>
            <label className="premium-seat-field"><span>探测模型（可选）</span><input value={draft.probeModel} placeholder="自动发现，无需填写"
              onChange={(e) => edit({ probeModel: e.target.value.trim() })} /><small>留空时从此 Key 与目标成员的模型目录自动选择；填写后仅使用指定模型验证。</small></label>
          </div>
          <div className="premium-seat-pool">
            <div className="premium-seat-section-heading"><h3><Users size={14} />候选账号池</h3><span className="codex-api-service-pill muted">{candidates.length} 位成员</span></div>
            <p className="premium-seat-hint">账号池内须恰好有一个高级席位，只调换池内成员；工作区可有其他高级席位。跟随池内顺序，从当前持席成员的下一位开始，最多检查一轮。</p>
            {candidates.length > 0 ? <ol className="premium-seat-members">{candidates.map((account, index) => <li key={account.id}>
              <span className="premium-seat-member-index">{index + 1}</span><span className="premium-seat-member-email" title={account.email}>{account.email}</span>
              {state?.holderAccountId === account.id && <span className="codex-api-service-pill success">当前持席</span>}
            </li>)}</ol> : <div className="premium-seat-empty"><Users size={22} /><strong>暂无候选成员</strong><span>{!draft.apiKeyId || !draft.workspaceId ? "选择管理员和 API Key 后显示账号池。" : "请在账号池中加入同工作区的团队 OAuth 成员。"}</span></div>}
            <p className="premium-seat-hint">排除暂停及官方授权模式的成员；后台还会核对模型权限、远端成员和席位状态。账号池变化后自动更新。</p>
          </div>
        </div>
        <label className="premium-seat-enable">
          <span><strong>启用自动调换</strong><small>只调换已有高级席位，不购买席位；调换不保证额度重置。</small></span>
          <input className="premium-seat-toggle" type="checkbox" role="switch" checked={draft.enabled} disabled={enableDisabled}
            onChange={(e) => { if (!enableDisabled) edit({ enabled: e.target.checked }); }} />
        </label>
        {draftEligibilityReason && <p className="premium-seat-hint" role="status">{draftEligibilityReason}</p>}
        {draft.enabled && <p className="premium-seat-hint">启用后，将在此 Key 当前及后续加入的同工作区池内成员中自动调换现有高级席位。变更会影响来源和目标成员在工作区及其他客户端的权益；初次启用和每次调换会消耗少量额度执行定向探测。</p>}
        <div className="premium-seat-form-footer"><span className="premium-seat-hint">只读核对不会变更席位。</span><div className="premium-seat-actions">
          <button type="button" className="btn btn-secondary btn-sm" disabled={checkDisabled} onClick={() => { if (!checkDisabled) void run(() => checkPremiumSeat(draft)); }}><ShieldCheck size={14} />只读核对</button>
          <button type="button" className="btn btn-primary btn-sm" disabled={saveDisabled} onClick={() => { if (!saveDisabled) void run(() => configurePremiumSeat(draft), true); }}><Save size={14} />保存配置</button>
        </div></div>
      </fieldset>
    </div>
    {state?.operation && <div className="premium-seat-operation"><span>最近调换</span><div>{display(state.operation.sourceAccountId)}<ArrowRight size={13} />{display(state.operation.targetAccountId)}</div><small>操作 ID：{state.operation.id}</small></div>}
    {(state?.message || notice) && <p className="premium-seat-feedback" role="status">{state?.message || notice}</p>}
    {unresolved && <div className="premium-seat-callout"><CircleAlert size={14} /><span>未决操作期间不能修改配置。重新核对先查询远端；只有资格满足时才继续验证和恢复，否则保持暂停。若仍为原分配，等待核对期结束后可确认结束该操作。</span></div>}
    {(enabled || unresolved) && <div className="premium-seat-actions premium-seat-runtime-actions">
      <button type="button" className="btn btn-secondary btn-sm" disabled={pauseDisabled} onClick={() => { if (!pauseDisabled) void run(() => controlPremiumSeat("pause")); }}><Pause size={14} />暂停调换</button>
      <button type="button" className="btn btn-secondary btn-sm" disabled={resumeDisabled} onClick={() => { if (!resumeDisabled) void run(() => controlPremiumSeat("resume")); }}><RefreshCw size={14} />{unresolved ? "重新核对未决操作" : "重新核对并恢复"}</button>
      {unresolved && <button type="button" className="btn btn-secondary btn-sm" disabled={busy || !state?.paused} onClick={() => { if (!busy && state?.paused) void run(() => controlPremiumSeat("resolve_unapplied")); }}>核对并确认未执行</button>}
    </div>}
    {busy && <p className="premium-seat-feedback" role="status"><RefreshCw size={13} className="loading-spinner" />正在处理，请稍候…</p>}
    {error && <div role="alert" className="premium-seat-callout premium-seat-error"><CircleAlert size={14} /><span>{error}</span></div>}
  </section>;
}
