//! Testable workflow; the production adapter owns OAuth, ACLs and upstream I/O.
use super::*;
use std::{
    future::Future,
    time::{Duration, Instant},
};

pub trait Backend: Sync {
    fn now(&self) -> i64;
    fn member(&self, account: &str) -> Result<String, String>;
    fn holder(&self, members: &[Member]) -> impl Future<Output = Result<String, String>> + Send;
    fn candidates(&self) -> impl Future<Output = Result<Vec<String>, String>> + Send;
    fn check_holder_scope(&self, account: &str) -> impl Future<Output = Result<(), String>> + Send;
    fn check_scope(&self) -> impl Future<Output = Result<(), String>> + Send;
    fn snapshot(&self) -> impl Future<Output = Result<http::Snapshot, http::AdminError>> + Send;
    fn quota(
        &self,
        account: &str,
    ) -> impl Future<Output = Result<(QuotaEvidence, Option<i64>), QuotaReadError>> + Send;
    fn probe(&self, account: &str) -> impl Future<Output = Result<(), String>> + Send;
    fn swap(&self, operation: &Operation) -> impl Future<Output = Result<(), String>> + Send;
}

pub fn pause_path(dir: &Path) -> PathBuf {
    dir.join("codex_premium_seat_rotation.pause")
}

fn validate_identities<B: Backend>(backend: &B, op: &Operation) -> Result<(), String> {
    for (id, user) in [
        (&op.source_account_id, &op.source_user_id),
        (&op.target_account_id, &op.target_user_id),
    ] {
        if backend.member(id)? != *user {
            return Err("操作期间本地成员身份已改变，已暂停".into());
        }
    }
    Ok(())
}

/// One bounded pass, starting after the current holder in the effective key pool.
pub fn circular_candidates(ids: &[String], holder: &str) -> Result<Vec<String>, String> {
    let index = ids
        .iter()
        .position(|id| id == holder)
        .ok_or("当前高级成员已不在有效账号池中")?;
    Ok(ids
        .iter()
        .cycle()
        .skip(index + 1)
        .take(ids.len() - 1)
        .cloned()
        .collect())
}

async fn validate_operation_scope<B: Backend>(backend: &B, op: &Operation) -> Result<(), String> {
    backend.check_scope().await?;
    let ids = backend.candidates().await?;
    if !ids.contains(&op.source_account_id) || !ids.contains(&op.target_account_id) {
        return Err("来源或目标成员未通过 Key 的账号池、授权或模型资格校验，已停止操作".into());
    }
    validate_identities(backend, op)
}

/// A completed exhaustion-triggered swap followed by rapid exhaustion requires
/// human review. Keep the record across resume, configuration edits and restart.
fn pause_on_rapid_exhaustion(config: &Config, state: &mut State, now: i64) -> bool {
    let recent = state
        .operation
        .iter()
        .chain(state.history.iter())
        .filter(|op| {
            op.workspace_id == config.workspace_id
                && op.submitted_at.is_some()
                && !op.resolved_without_swap
                && op.exhaustion_triggered != Some(false)
        })
        .filter_map(|op| op.verified_at)
        .max()
        .is_some_and(|at| now.saturating_sub(at) < RAPID_EXHAUSTION_WINDOW_SECS);
    if recent {
        if state.pending() {
            state.cancel_prepared();
        }
        state.attention(format!(
            "{} 分钟内已因额度耗尽完成一次席位轮换，当前成员再次耗尽；已停止轮换，避免循环。请等待额度恢复后手动核对并恢复",
            RAPID_EXHAUSTION_WINDOW_SECS / 60
        ));
        state.next_check_at = None;
    }
    recent
}

pub async fn step<B: Backend>(
    dir: &Path,
    config: &Config,
    state: &mut State,
    backend: &B,
) -> Result<(), String> {
    if can_recheck_paused_holder(state) && state.phase == Phase::NeedsAttention {
        // Older paused monitors stopped permanently when quota was unknown.
        // Recover only that read-only state, retaining the user's rotation pause.
        state.phase = Phase::Idle;
    } else if can_retry_monitor_read(dir, state) {
        state.paused = false;
        state.phase = Phase::Idle;
        state.next_check_at = None;
    }
    step_with_trigger(dir, config, state, backend, false).await
}

/// Recover pre-fix read failures, never a user pause or uncertain swap.
pub fn can_retry_monitor_read(dir: &Path, state: &State) -> bool {
    state.paused
        && state.phase == Phase::NeedsAttention
        && !state.pending()
        && !pause_path(dir).exists()
        && state.config.as_ref().is_some_and(|config| config.enabled)
        && state.holder_account_id.is_some()
        && state.holder_user_id.is_some()
        && state.message.as_deref().is_some_and(|message| {
            http::AdminError::legacy_connection_failure(message)
                || message == "实时订阅窗口无法判断；未提交席位调换"
        })
}

/// Full verification may refresh administrator authorization. A paused holder's
/// quota-only monitoring never calls the administrator API.
pub fn can_refresh_admin(state: &State) -> bool {
    !state.paused || can_recheck_paused_holder(state)
}

pub fn can_recheck_paused_holder(state: &State) -> bool {
    state.paused
        && !state.pending()
        && state.config.as_ref().is_some_and(|config| config.enabled)
        && state.holder_account_id.is_some()
        && state.holder_user_id.is_some()
        && (matches!(state.phase, Phase::Ready | Phase::Idle)
            || (state.phase == Phase::NeedsAttention
                && state.message.as_deref() == Some("调换已暂停，当前成员未确认额度可用")))
}

async fn monitor_snapshot<B: Backend>(
    backend: &B,
    state: &mut State,
    retry: bool,
) -> Result<Option<http::Snapshot>, String> {
    match backend.snapshot().await {
        Ok(snapshot) => Ok(Some(snapshot)),
        Err(error) if retry && error.retryable() => {
            // Failed reads cannot establish a new holder or authorize a mutation.
            state.message = Some(format!("只读核对暂时失败，稍后重试：{error}"));
            state.next_check_at = Some(backend.now() + MONITOR_RETRY_SECS);
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

async fn verify_holder<B: Backend>(
    backend: &B,
    state: &mut State,
    holder: &str,
    counts: &BTreeMap<String, u64>,
) -> Result<bool, String> {
    if state.holder_account_id.as_deref() != Some(holder) || state.validated_at.is_none() {
        backend.probe(holder).await?;
        let Some((after_probe, after_counts)) = monitor_snapshot(backend, state, true).await?
        else {
            return Ok(false);
        };
        if backend.holder(&after_probe).await? != holder || &after_counts != counts {
            return Err("初次验证期间席位分配或数量已变化，拒绝发布路由".into());
        }
    }
    backend.check_scope().await?;
    if !backend.candidates().await?.iter().any(|id| id == holder) {
        return Err("当前高级成员已离开有效账号池".into());
    }
    Ok(true)
}

/// Reuse the last verified holder while its local scope is intact. Only quota
/// exhaustion reaches the remote seat checks below; monitoring is not a lease
/// that a management API outage can expire.
async fn monitor_verified_holder<B: Backend>(
    dir: &Path,
    config: &Config,
    state: &mut State,
    backend: &B,
) -> Result<bool, String> {
    let holder = state.holder_account_id.clone().ok_or("持席成员缺失")?;
    if Some(backend.member(&holder)?) != state.holder_user_id {
        return Err("已验证的持席成员身份已变化，请核对后恢复".into());
    }
    backend.check_holder_scope(&holder).await?;
    let (evidence, reset) = match backend.quota(&holder).await {
        Ok(quota) => quota,
        Err(error) if error.retryable() => {
            state.message = Some(format!(
                "额度刷新暂时失败，保留当前持席状态，稍后重试：{}",
                String::from(error)
            ));
            state.next_check_at = Some(backend.now() + MONITOR_RETRY_SECS);
            return Ok(true);
        }
        Err(error) => return Err(error.into()),
    };
    let paused = state.paused || pause_path(dir).exists();
    state.paused = paused;
    match evidence {
        QuotaEvidence::Available => {
            state.phase = Phase::Ready;
            state.exhausted_until.remove(&holder);
            state.message = Some(
                if paused {
                    "调换已暂停，当前高级成员已验证，正在监测实时订阅额度"
                } else {
                    "高级成员已验证，正在监测实时订阅额度"
                }
                .into(),
            );
            state.next_check_at = Some(backend.now() + MONITOR_CHECK_SECS);
            Ok(true)
        }
        QuotaEvidence::Unknown => {
            // Unknown is not exhaustion. An exhausted holder also stays blocked
            // until a later read positively confirms availability.
            state.message =
                Some("实时额度暂时未知，保留当前持席状态，稍后重试；未提交席位调换".into());
            state.next_check_at = Some(backend.now() + MONITOR_RETRY_SECS);
            Ok(true)
        }
        QuotaEvidence::Exhausted => {
            state.phase = Phase::Idle;
            state
                .exhausted_until
                .insert(holder, reset.ok_or("耗尽窗口缺少重置时间")?);
            if paused {
                state.paused = true;
                state.message = Some("调换已暂停，当前成员额度已耗尽，等待额度恢复".into());
                state.next_check_at = Some(backend.now() + MONITOR_RETRY_SECS);
                return Ok(true);
            }
            if pause_on_rapid_exhaustion(config, state, backend.now()) {
                return Ok(true);
            }
            if state
                .operation
                .as_ref()
                .and_then(|op| op.submitted_at)
                .is_some_and(|at| backend.now().saturating_sub(at) < MIN_SWAP_INTERVAL_SECS)
            {
                state.message = Some(format!(
                    "额度耗尽，等待最短调换间隔（{} 秒）；平台限制仍优先",
                    MIN_SWAP_INTERVAL_SECS
                ));
                state.next_check_at = Some(backend.now() + MONITOR_CHECK_SECS);
                return Ok(true);
            }
            Ok(false)
        }
    }
}

pub fn check_manual_next(
    dir: &Path,
    config: &Config,
    state: &State,
    now: i64,
) -> Result<(), String> {
    if !config.enabled || state.config.as_ref() != Some(config) {
        return Err("请先保存并启用席位调换配置".into());
    }
    if state.pending() {
        return Err("已有未完成的席位操作，请先核对结果".into());
    }
    if state.paused || pause_path(dir).exists() {
        return Err("席位调换已暂停，请先重新核对并恢复".into());
    }
    let holder = state
        .holder_account_id
        .as_deref()
        .ok_or("当前高级成员尚未验证")?;
    if !state.allows(Some(&config.api_key_id), holder, false, now) {
        return Err("当前高级成员尚未验证或额度已不可用，请先重新核对并恢复".into());
    }
    if state
        .operation
        .as_ref()
        .and_then(|op| op.submitted_at)
        .is_some_and(|at| now.saturating_sub(at) < MIN_SWAP_INTERVAL_SECS)
    {
        return Err(format!(
            "尚未达到最短调换间隔（{} 秒）",
            MIN_SWAP_INTERVAL_SECS
        ));
    }
    Ok(())
}

/// One explicit attempt; no queued intent survives a failed preparation.
pub async fn switch_next<B: Backend>(
    dir: &Path,
    config: &Config,
    state: &mut State,
    backend: &B,
) -> Result<(), String> {
    check_manual_next(dir, config, state, backend.now())?;
    let result = step_with_trigger(dir, config, state, backend, true).await;
    if let Err(error) = &result {
        state.attention(error.clone());
    }
    result
}

async fn step_with_trigger<B: Backend>(
    dir: &Path,
    config: &Config,
    state: &mut State,
    backend: &B,
    manual: bool,
) -> Result<(), String> {
    if !manual
        && !state.pending()
        && state.validated_at.is_some()
        && matches!(state.phase, Phase::Ready | Phase::Idle)
        && monitor_verified_holder(dir, config, state, backend).await?
    {
        return Ok(());
    }
    // A submitted operation is still readable after its team/key eligibility is
    // revoked. Eligibility is checked again before any probe or publication.
    if !state.pending() {
        backend.check_scope().await?;
    }
    let retry_read = !manual && !state.pending();
    let Some((members, counts)) = monitor_snapshot(backend, state, retry_read).await? else {
        return Ok(());
    };
    if state.pending() {
        let op = state.operation.clone().ok_or("席位操作缺失")?;
        validate_identities(backend, &op)?;
        if op.submitted_at.is_none() {
            // A crash before POST cancels preparation; never resurrect a stale drain.
            state.cancel_prepared();
            state.attention("上次任务在提交前中断，席位未提交变更，请重新核对后恢复");
            return Ok(());
        }
        return match reconcile(&op, &members, &counts) {
            Pair::After => {
                state.phase = Phase::Verifying;
                if state.paused {
                    state
                        .message
                        .get_or_insert_with(|| "远端已完成调换，等待恢复后验证目标成员".into());
                    return Ok(());
                }
                if backend.holder(&members).await? != op.target_account_id {
                    return Err("未决操作的目标不再是账号池内唯一高级成员，拒绝验证".into());
                }
                verify_target(dir, config, state, backend).await
            }
            Pair::Before => {
                state.attention("已提交操作尚未观察到调换结果；不会重发，请在成员管理中核对后恢复");
                Ok(())
            }
            Pair::Conflict => {
                Err("远端席位或数量与操作记录冲突，已暂停；不会覆盖管理员变更".into())
            }
        };
    }
    let holder = backend.holder(&members).await?;
    if state
        .holder_account_id
        .as_ref()
        .is_some_and(|old| old != &holder)
    {
        return Err("管理员已改变高级席位分配，请核对并手动恢复".into());
    }
    let (evidence, reset) = match backend.quota(&holder).await {
        Ok(quota) => quota,
        Err(error) if !manual && error.retryable() => {
            // Initial validation and switching still require a successful read.
            state.message = Some(format!(
                "只读额度核对暂时失败，稍后重试：{}",
                String::from(error)
            ));
            state.next_check_at = Some(backend.now() + MONITOR_RETRY_SECS);
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if state.paused || pause_path(dir).exists() {
        if can_recheck_paused_holder(state) || (state.phase == Phase::Ready && !state.pending()) {
            match evidence {
                QuotaEvidence::Available => {
                    if !verify_holder(backend, state, &holder, &counts).await? {
                        return Ok(());
                    }
                    state.validated_at = Some(backend.now());
                    state.phase = Phase::Ready;
                    state.message =
                        Some("调换已暂停，当前高级成员已验证，正在监测实时订阅额度".into());
                    state.next_check_at = Some(backend.now() + MONITOR_CHECK_SECS);
                }
                QuotaEvidence::Unknown | QuotaEvidence::Exhausted => {
                    // Without retained verification, legacy paused holders stay
                    // unpublished until a full check succeeds.
                    state.validated_at = None;
                    state.phase = Phase::Idle;
                    state.message = Some("调换已暂停，当前成员额度未确认可用，稍后自动核对".into());
                    state.next_check_at = Some(backend.now() + MONITOR_RETRY_SECS);
                }
            }
        } else {
            state.attention("调换已暂停，当前成员未确认额度可用");
        }
        return Ok(());
    }
    match evidence {
        QuotaEvidence::Unknown => {
            if manual {
                return Err("实时订阅窗口无法判断；未提交席位调换".into());
            }
            state.phase = Phase::Idle;
            state.message = Some("实时订阅窗口无法判断；稍后重新核对，未提交席位调换".into());
            state.next_check_at = Some(backend.now() + MONITOR_RETRY_SECS);
            return Ok(());
        }
        QuotaEvidence::Available if !manual => {
            if !verify_holder(backend, state, &holder, &counts).await? {
                return Ok(());
            }
            if pause_path(dir).exists() {
                return Ok(());
            }
            state.holder_account_id = Some(holder);
            state
                .exhausted_until
                .remove(state.holder_account_id.as_deref().ok_or("持席成员缺失")?);
            state.holder_user_id =
                Some(backend.member(state.holder_account_id.as_deref().ok_or("持席成员缺失")?)?);
            state.validated_at = Some(backend.now());
            state.phase = Phase::Ready;
            state.message = Some("高级成员已验证，正在监测实时订阅额度".into());
            state.next_check_at = Some(backend.now() + MONITOR_CHECK_SECS);
            return Ok(());
        }
        QuotaEvidence::Available | QuotaEvidence::Exhausted => {}
    }
    if evidence == QuotaEvidence::Exhausted {
        state.phase = Phase::Idle;
        state.holder_account_id = Some(holder.clone());
        let reset = reset.ok_or("耗尽窗口缺少重置时间")?;
        state.exhausted_until.insert(holder.clone(), reset);
        if pause_on_rapid_exhaustion(config, state, backend.now()) {
            return Ok(());
        }
    }
    if state
        .operation
        .as_ref()
        .and_then(|o| o.submitted_at)
        .is_some_and(|at| backend.now() - at < MIN_SWAP_INTERVAL_SECS)
    {
        state.message = Some(format!(
            "额度耗尽，等待最短调换间隔（{} 秒）；平台限制仍优先",
            MIN_SWAP_INTERVAL_SECS
        ));
        state.next_check_at = Some(backend.now() + MONITOR_CHECK_SECS);
        return Ok(());
    }
    let mut target = None;
    let mut excluded = Vec::new();
    let ids = backend.candidates().await?;
    let order = if manual || config.use_api_key_pool {
        circular_candidates(&ids, &holder)?
    } else {
        ids
    };
    for id in &order {
        if id == &holder {
            continue;
        }
        let user = backend.member(id)?;
        if !members
            .iter()
            .any(|member| member.id == user && member.eligible() && member.seat_type == "default")
        {
            excluded.push(format!("{id}: 当前席位或成员状态不满足条件"));
            continue;
        }
        if let Some(at) = state.exhausted_until.get(id) {
            if *at > backend.now() {
                excluded.push(format!("{id}: 已知额度窗口尚未重置"));
                continue;
            }
        }
        // Check valid authorization now. A standard seat's current zero balance
        // does not predict its premium entitlement after swapping.
        let quota = match backend.quota(id).await {
            Ok(quota) => quota,
            Err(_) => {
                excluded.push(format!("{id}: 授权或实时额度查询不可用"));
                continue;
            }
        };
        if state.exhausted_until.contains_key(id) && quota.0 != QuotaEvidence::Available {
            excluded.push(format!("{id}: 尚未确认额度恢复"));
            continue;
        }
        target = Some((id.clone(), user));
        break;
    }
    let Some(target) = target else {
        if manual {
            state.message = Some(format!(
                "暂无可用的下一位成员，未调换席位。{}",
                excluded.join("；")
            ));
            state.next_check_at = Some(backend.now() + MONITOR_CHECK_SECS);
            return Ok(());
        }
        state.message = Some(format!(
            "额度耗尽，暂无可用候选，保留现有席位。{}",
            excluded.join("；")
        ));
        state.next_check_at = Some(backend.now() + MONITOR_CHECK_SECS);
        return Ok(());
    };
    let source = backend.member(&holder)?;
    if let Some(previous) = state.operation.take() {
        state.history.push(previous);
        if state.history.len() > 50 {
            state.history.remove(0);
        }
    }
    state.operation = Some(Operation {
        id: uuid::Uuid::new_v4().to_string(),
        workspace_id: config.workspace_id.clone(),
        source_account_id: holder.clone(),
        target_account_id: target.0.clone(),
        source_user_id: source,
        target_user_id: target.1,
        before_counts: counts,
        created_at: backend.now(),
        submitted_at: None,
        exhaustion_triggered: None,
        probe_started: false,
        verified_at: None,
        resolved_without_swap: false,
    });
    state.phase = Phase::Draining;
    state.next_check_at = None;
    save(dir, state)?;
    let deadline = Instant::now() + Duration::from_secs(DRAIN_TIMEOUT_SECS);
    let _drain = loop {
        if pause_path(dir).exists() {
            state.cancel_prepared();
            state.paused = true;
            return Ok(());
        }
        match drain_lock(dir, &holder) {
            Ok(lock) => break lock,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await
            }
            Err(_) => {
                state.cancel_prepared();
                return Err("等待在途请求超时，已取消尚未提交的调换".into());
            }
        }
    };
    // Recheck both local authorization and remote state after draining.
    validate_operation_scope(backend, state.operation.as_ref().ok_or("操作缺失")?).await?;
    let (latest_members, latest_counts) = backend.snapshot().await?;
    if reconcile(
        state.operation.as_ref().ok_or("席位操作缺失")?,
        &latest_members,
        &latest_counts,
    ) != Pair::Before
        || backend.holder(&latest_members).await? != holder
    {
        return Err("等待期间远端席位已变化，取消自动提交".into());
    }
    let (latest_quota, latest_reset) = backend.quota(&holder).await?;
    if manual && latest_quota == QuotaEvidence::Unknown {
        state.cancel_prepared();
        return Err("提交前无法确认来源成员的实时额度，已取消手动调换".into());
    }
    if manual && latest_quota == QuotaEvidence::Exhausted {
        let Some(reset) = latest_reset else {
            state.cancel_prepared();
            return Err("来源成员耗尽窗口缺少重置时间，已取消手动调换".into());
        };
        state.exhausted_until.insert(holder.clone(), reset);
    }
    if !manual && latest_quota != QuotaEvidence::Exhausted {
        state.cancel_prepared();
        state.message = Some("额度已恢复或不再确定耗尽，取消调换".into());
        return Ok(());
    }
    if latest_quota == QuotaEvidence::Exhausted
        && pause_on_rapid_exhaustion(config, state, backend.now())
    {
        return Ok(());
    }
    validate_operation_scope(backend, state.operation.as_ref().ok_or("操作缺失")?).await?;
    if pause_path(dir).exists() {
        state.cancel_prepared();
        state.paused = true;
        return Ok(());
    }
    let operation = state.operation.as_mut().ok_or("席位操作缺失")?;
    operation.exhaustion_triggered = Some(latest_quota == QuotaEvidence::Exhausted);
    operation.submitted_at = Some(backend.now());
    state.phase = Phase::Submitted;
    save(dir, state)?;
    if pause_path(dir).exists() {
        return Ok(());
    }
    let post_error = backend
        .swap(state.operation.as_ref().ok_or("席位操作缺失")?)
        .await
        .err();
    let (after, after_counts) = backend.snapshot().await?;
    match reconcile(
        state.operation.as_ref().ok_or("席位操作缺失")?,
        &after,
        &after_counts,
    ) {
        Pair::After => {
            state.phase = Phase::Verifying;
            save(dir, state)?;
            if pause_path(dir).exists() {
                state.paused = true;
                return Ok(());
            }
            if backend.holder(&after).await? != target.0 {
                return Err("调换后目标不再是账号池内唯一高级成员，拒绝验证".into());
            }
            verify_target(dir, config, state, backend).await
        }
        Pair::Before => {
            Err(post_error.unwrap_or_else(|| "调换提交后尚未观察到结果；已暂停，不会重发".into()))
        }
        Pair::Conflict => Err("调换后席位或数量冲突，已暂停，不自动补偿或购买".into()),
    }
}

async fn verify_target<B: Backend>(
    dir: &Path,
    _config: &Config,
    state: &mut State,
    backend: &B,
) -> Result<(), String> {
    let op = state.operation.clone().ok_or("验证操作缺失")?;
    validate_operation_scope(backend, &op).await?;
    let (evidence, _) = backend.quota(&op.target_account_id).await?;
    if evidence != QuotaEvidence::Available {
        if backend.now() - op.submitted_at.unwrap_or(op.created_at) < VERIFY_TIMEOUT_SECS {
            state.message = Some("席位已调换，等待目标成员实时额度生效；暂不发布路由".into());
            state.next_check_at = Some(backend.now() + MONITOR_TICK_SECS as i64);
            return Ok(());
        }
        return Err("调换后目标成员额度未通过验证，已暂停；不会连续调换给其他成员".into());
    }
    if op.probe_started {
        return Err("上次业务探测未确认完成，请核对后手动恢复；不会自动重复探测".into());
    }
    state
        .operation
        .as_mut()
        .ok_or("验证操作缺失")?
        .probe_started = true;
    save(dir, state)?;
    backend.probe(&op.target_account_id).await?;
    validate_operation_scope(backend, &op).await?;
    let (members, counts) = backend.snapshot().await?;
    if reconcile(&op, &members, &counts) != Pair::After
        || backend.holder(&members).await? != op.target_account_id
    {
        return Err("验证期间席位分配发生变化，拒绝发布路由".into());
    }
    if backend.quota(&op.target_account_id).await?.0 != QuotaEvidence::Available {
        return Err("目标成员在业务探测后不再确认额度可用，拒绝发布路由".into());
    }
    validate_operation_scope(backend, &op).await?;
    if pause_path(dir).exists() {
        state.paused = true;
        return Ok(());
    }
    state.operation.as_mut().ok_or("验证操作缺失")?.verified_at = Some(backend.now());
    state.holder_account_id = Some(op.target_account_id);
    state.holder_user_id = Some(op.target_user_id);
    state.validated_at = Some(backend.now());
    state.phase = Phase::Ready;
    state.message = Some("席位、身份、实时额度与定向请求均验证成功，已切换有效路由".into());
    state.next_check_at = Some(backend.now() + MONITOR_CHECK_SECS);
    Ok(())
}
