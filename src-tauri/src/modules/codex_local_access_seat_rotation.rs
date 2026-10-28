// Included in codex_local_access so the coordinator uses the same ACL and probe path.
use crate::modules::codex_seat_rotation as seat;

static SEAT_MONITOR_STARTED: AtomicBool = AtomicBool::new(false);

fn seat_dir() -> Result<PathBuf, String> {
    account::get_data_dir()
}
fn seat_now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn seat_pause_path(dir: &Path) -> PathBuf {
    dir.join("codex_premium_seat_rotation.pause")
}

pub fn get_premium_seat_rotation() -> Result<seat::State, String> {
    seat::load(&seat_dir()?)
}

async fn seat_collection(config: &seat::Config) -> Result<CodexLocalAccessCollection, String> {
    ensure_runtime_loaded_without_start().await?;
    let collection = gateway_runtime()
        .lock()
        .await
        .collection
        .clone()
        .ok_or("API 服务尚未配置")?;
    if !collection.enabled || collection.gateway_mode != CodexLocalAccessGatewayMode::Native {
        return Err("高级席位调换需要已启用的原生 Rust API 网关".into());
    }
    let key = collection
        .api_keys
        .iter()
        .find(|k| k.id == config.api_key_id && k.enabled)
        .ok_or("选中的 API Key 不存在或已禁用")?;
    // Initial version intentionally supports a single OAuth pool. Provider and
    // model-routing keys need a separate scope policy before they can be managed.
    if key.model_routing.is_some() || key.provider_gateway.is_some() {
        return Err("初版席位调换只支持普通 OAuth 账号池 Key".into());
    }
    Ok(collection)
}

fn seat_probe_model(
    config: &seat::Config,
    collection: &CodexLocalAccessCollection,
    key: &CodexLocalAccessApiKey,
    account: &CodexAccount,
) -> Result<String, String> {
    // Use the native service's existing catalog, scoped to this exact member.
    // No image-model append, cross-account fallback, or paid trial enumeration.
    let discovered = if config.probe_model.trim().is_empty() {
        let catalog = native_catalog(collection, Some(key), std::slice::from_ref(account))
            .map_err(|_| "无法读取目标成员的模型目录")?;
        catalog["models"]
            .as_array()
            .ok_or("目标成员的模型目录格式无效")?
            .iter()
            .filter(|model| model["visibility"] != "hide")
            .filter_map(|model| model["slug"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    seat::select_probe_model(&config.probe_model, discovered, |exposed| {
        let model = native_model_for_key(collection, Some(key), exposed).ok()?;
        let plan = native_model_route_plan(collection, Some(key), &model).ok()?;
        if plan.provider || !plan.account_ids.contains(&account.id) {
            return None;
        }
        let upstream = native_operation_model(collection, &plan, account, None, false)?;
        if (config.probe_model.trim().is_empty() && native_is_reserve_model(&upstream))
            || native_excluded(&collection.excluded_models, &upstream)
            || native_excluded(&key.excluded_models, &upstream)
            || collection.account_model_rules.iter().any(|rule| {
                rule.account_id == account.id
                    && [
                        exposed,
                        model.as_str(),
                        plan.model.as_str(),
                        upstream.as_str(),
                    ]
                    .iter()
                    .any(|name| native_excluded(&rule.excluded_models, name))
            })
        {
            return None;
        }
        Some(model)
    })
}

fn seat_candidate_ids(
    config: &seat::Config,
    collection: &CodexLocalAccessCollection,
) -> Result<Vec<String>, String> {
    let key = collection
        .api_keys
        .iter()
        .find(|k| k.id == config.api_key_id && k.enabled)
        .ok_or("选中的 API Key 不存在或已禁用")?;
    let pool = native_scoped_account_ids(collection, Some(key));
    let requested = if config.use_api_key_pool {
        &pool
    } else {
        &config.member_account_ids
    };
    let local_accounts =
        codex_account::list_accounts_checked().map_err(|_| "无法完整读取本地账号")?;
    let mut candidates = Vec::new();
    let mut users = HashSet::new();
    for id in requested {
        let eligible = (|| -> Result<CodexAccount, String> {
            if !pool.contains(id) {
                return Err("成员超出了 API Key 当前有效账号池".into());
            }
            if collection.official_account_bindings.contains_key(id) {
                return Err("席位调换需要成员使用本地 OAuth 接入方式".into());
            }
            let account = seat_team_member_account(config, id)?;
            if local_accounts.iter().any(|other| {
                other.id != account.id
                    && !other.is_api_key_auth()
                    && !other.is_agent_identity_auth()
                    && other.account_id == account.account_id
                    && other.user_id == account.user_id
            }) {
                return Err("工作区成员存在重复本地授权，无法完整排空在途请求".into());
            }
            seat_probe_model(config, collection, key, &account)?;
            Ok(account)
        })();
        match eligible {
            Ok(account) if users.insert(account.user_id.clone()) => candidates.push(id.clone()),
            Ok(_) if !config.use_api_key_pool => return Err("多个本地账号对应同一成员".into()),
            Err(error) if !config.use_api_key_pool => return Err(error),
            _ => {}
        }
    }
    Ok(candidates)
}

fn seat_check_holder_scope(
    config: &seat::Config,
    collection: &CodexLocalAccessCollection,
    id: &str,
) -> Result<(), String> {
    if !config.use_api_key_pool && !config.member_account_ids.iter().any(|member| member == id) {
        return Err("当前高级成员已离开配置的授权账号池".into());
    }
    let account = seat_team_member_account(config, id)?;
    if !native_eligible(&account) {
        return Err("当前高级成员授权无法用于原生模型请求".into());
    }
    // A dormant candidate's missing credentials or model access must not stop
    // the current holder. Reuse all pool/model/identity checks for this member
    // only; the complete candidate set is checked when a switch is prepared.
    let mut holder_scope = config.clone();
    holder_scope.use_api_key_pool = false;
    holder_scope.member_account_ids = vec![id.to_string()];
    seat_candidate_ids(&holder_scope, collection).map(|_| ())
}

fn seat_member_account(config: &seat::Config, id: &str) -> Result<CodexAccount, String> {
    let account = codex_account::load_account(id).ok_or("席位成员的本地账号已不存在")?;
    if account.is_api_key_auth()
        || account.is_agent_identity_auth()
        || account.is_web_session_auth()
        || native_is_chatgpt_plan_account(&account)
        || account.account_id.as_deref() != Some(config.workspace_id.as_str())
        || account.user_id.as_deref().is_none_or(str::is_empty)
    {
        return Err("席位成员必须是同一工作区、具备明确成员 ID 的 OAuth 账号".into());
    }
    Ok(account)
}

fn seat_require_team_identity(account: &CodexAccount) -> Result<(), String> {
    if !seat::has_team_identity(
        account.plan_type.as_deref(),
        account.account_structure.as_deref(),
    ) {
        return Err("高级席位自动调换仅限已确认的团队工作区账号；个人账号或团队身份未知时不可启用，请先刷新账号资料".into());
    }
    Ok(())
}

fn seat_team_member_account(config: &seat::Config, id: &str) -> Result<CodexAccount, String> {
    let account = seat_member_account(config, id)?;
    seat_require_team_identity(&account)?;
    Ok(account)
}

async fn seat_confirm_team_member(config: &seat::Config, id: &str) -> Result<CodexAccount, String> {
    let original = seat_team_member_account(config, id)?;
    seat_confirm_team_profile(original).await
}

// Callers establish a team candidate before preparing/refreshing credentials.
// The refresh itself may change the plan; only the real workspace profile may
// establish its stable structure, and explicit personal metadata still wins.
async fn seat_confirm_team_profile(original: CodexAccount) -> Result<CodexAccount, String> {
    if seat::has_personal_structure(original.account_structure.as_deref()) {
        return Err("个人工作区不能启用或继续高级席位调换".into());
    }
    if seat::has_team_identity(None, original.account_structure.as_deref()) {
        return Ok(original);
    }
    // A seat-specific quota response may replace `team` with `prolite`. Fill the
    // existing workspace-structure field from its real profile before that can
    // happen; never invent a persistent trust marker from a plan name.
    let refreshed = codex_account::refresh_account_profile(&original.id)
        .await
        .map_err(|_| "无法核对团队工作区资料，请先刷新账号资料后重试")?;
    if refreshed.account_id != original.account_id || refreshed.user_id != original.user_id {
        return Err("核对团队资料时成员或工作区身份发生变化，已停止席位操作".into());
    }
    if !seat::has_team_identity(None, refreshed.account_structure.as_deref()) {
        return Err("远端资料未确认团队工作区结构，不能启用或继续高级席位调换".into());
    }
    Ok(refreshed)
}

async fn seat_admin_api(config: &seat::Config) -> Result<seat::http::AdminApi, seat::http::AdminError> {
    let admin = timeout(
        Duration::from_secs(30),
        codex_account::prepare_account_for_quota_query(&config.admin_account_id),
    )
    .await
    .map_err(|_| seat::http::AdminError::connection("管理员授权准备超时"))?
    .map_err(|_| "无法准备管理员授权，请重新登录")?;
    seat_admin_api_for_account(config, &admin)
        .await
        .map_err(Into::into)
}

async fn seat_admin_api_for_account(
    config: &seat::Config,
    admin: &CodexAccount,
) -> Result<seat::http::AdminApi, String> {
    if admin.is_api_key_auth()
        || admin.is_agent_identity_auth()
        || admin.is_web_session_auth()
        || admin.account_id.as_deref() != Some(config.workspace_id.as_str())
    {
        return Err("管理员必须使用目标工作区的 OAuth 授权".into());
    }
    let builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never());
    let client = crate::modules::codex_proxy_runtime::client_builder(admin, builder)
        .await
        .map_err(|_| "无法准备管理员网络连接")?
        .build()
        .map_err(|_| "无法创建管理员网络连接")?;
    seat::http::AdminApi::new(
        client,
        &admin.tokens.access_token,
        &config.workspace_id,
        admin.token_generation,
    )
}

async fn seat_admin_snapshot(
    config: &seat::Config,
    api: &mut seat::http::AdminApi,
) -> Result<seat::http::Snapshot, seat::http::AdminError> {
    api.snapshot_with_refresh(|observed| async move {
        let original = seat_member_account(config, &config.admin_account_id)?;
        let refreshed = codex_account::force_refresh_managed_account_after_observed(
            &config.admin_account_id,
            observed,
            "premium-seat-admin-read-401",
        )
        .await
        .map_err(|_| "管理员只读查询返回 401，凭据刷新失败；请重新授权管理员账号")?;
        if refreshed.account_id != original.account_id || refreshed.user_id != original.user_id {
            return Err("刷新后管理员身份发生变化，已停止成员查询".into());
        }
        seat_admin_api_for_account(config, &refreshed).await
    })
    .await
}

async fn seat_read_snapshot(config: &seat::Config) -> Result<seat::http::Snapshot, String> {
    let mut api = seat_admin_api(config).await?;
    seat_admin_snapshot(config, &mut api)
        .await
        .map_err(Into::into)
}

fn seat_local_holder(
    config: &seat::Config,
    collection: &CodexLocalAccessCollection,
    members: &[seat::Member],
    ids: &[String],
) -> Result<String, String> {
    let admin = seat_team_member_account(config, &config.admin_account_id)?;
    if !members.iter().any(|m| {
        Some(m.id.as_str()) == admin.user_id.as_deref()
            && m.eligible()
            && matches!(
                m.role.as_str(),
                "account-owner" | "account-admin" | "owner" | "admin"
            )
    }) {
        return Err("管理员身份未匹配工作区的有效 Owner/Admin 成员".into());
    }
    let key = collection
        .api_keys
        .iter()
        .find(|key| key.id == config.api_key_id && key.enabled)
        .ok_or("选中的 API Key 不存在或已禁用")?;
    let pool_user_ids = native_scoped_account_ids(collection, Some(key))
        .iter()
        .filter_map(|id| codex_account::load_account(id))
        .filter(|account| account.account_id.as_deref() == Some(config.workspace_id.as_str()))
        .filter_map(|account| account.user_id)
        .collect::<Vec<_>>();
    let premium = seat::pool_premium_holder(members, &pool_user_ids)?;
    let mut holder = None;
    for id in ids {
        let account = seat_team_member_account(config, id)?;
        let Some(member) = members
            .iter()
            .find(|m| Some(m.id.as_str()) == account.user_id.as_deref())
        else {
            if config.use_api_key_pool {
                continue;
            }
            return Err("本地成员 ID 与工作区成员列表不匹配".into());
        };
        if member.id == premium.id && member.eligible() {
            holder = Some(id.clone());
        }
    }
    holder.ok_or_else(|| "当前高级席位成员未包含在配置的授权账号池中".into())
}

async fn seat_fresh_quota(
    config: &seat::Config,
    id: &str,
) -> Result<(seat::QuotaEvidence, Option<i64>), seat::QuotaReadError> {
    let original = seat_confirm_team_member(config, id).await?;
    let mut account = timeout(
        Duration::from_secs(30),
        codex_account::prepare_account_for_quota_query(id),
    )
    .await
    .map_err(|_| seat::QuotaReadError::temporary("成员授权准备超时"))?
    .map_err(|error| {
        let current = codex_account::load_account(id);
        seat_quota_preparation_error(
            &original,
            current.as_ref(),
            &error,
            &crate::modules::config::get_user_config().language,
        )
    })?;
    if account.account_id != original.account_id || account.user_id != original.user_id {
        return Err("成员身份在刷新授权时发生变化".into());
    }
    seat_require_team_identity(&account)?;
    if !native_eligible(&account) || account.is_api_key_auth() || account.is_agent_identity_auth() {
        return Err("成员授权当前无法用于原生模型请求".into());
    }
    let started = Instant::now();
    let first = timeout(Duration::from_secs(30), codex_quota::fetch_quota(&account))
        .await
        .map_err(|_| seat::QuotaReadError::temporary("实时额度查询超时"))?;
    let result = match first {
        Ok(result) => result,
        Err(error) if seat::expired_member_access_token(&error) => {
            account = seat_refresh_member_access(config, id, account.token_generation).await?;
            timeout(Duration::from_secs(30), codex_quota::fetch_quota(&account))
                .await
                .map_err(|_| seat::QuotaReadError::temporary("刷新成员凭据后额度查询超时"))?
                .map_err(|error| seat::QuotaReadError::from_fetch(&error))?
        }
        Err(error) => return Err(seat::QuotaReadError::from_fetch(&error)),
    };
    // fetch_quota rejects conflicting response identities. Also verify the OAuth
    // principal against the expected member before accepting absent response IDs.
    let claims = account
        .tokens
        .access_token
        .split('.')
        .nth(1)
        .and_then(|p| general_purpose::URL_SAFE_NO_PAD.decode(p).ok())
        .and_then(|p| serde_json::from_slice::<Value>(&p).ok())
        .ok_or("无法核对成员令牌身份")?;
    let auth = claims
        .get("https://api.openai.com/auth")
        .ok_or("令牌缺少工作区身份")?;
    if auth.get("chatgpt_account_id").and_then(Value::as_str) != Some(config.workspace_id.as_str())
        || auth.get("chatgpt_user_id").and_then(Value::as_str) != account.user_id.as_deref()
    {
        return Err("成员令牌与工作区身份不一致".into());
    }
    let evidence = seat_live_quota_evidence(
        result.plan_type.as_deref(),
        result.quota.raw_data.as_ref(),
        seat_now(),
    )?;
    if evidence.0 == seat::QuotaEvidence::Unknown {
        // An incomplete read must not erase the last known plan/window and
        // indirectly invalidate a holder that is still serving requests.
        return Ok(evidence);
    }
    let current =
        codex_account::save_refreshed_quota(&account, result.quota, result.plan_type, started)
            .map_err(|_| "额度查询期间成员授权发生变化，请重试")?;
    // A quota refresh can change the plan, and concurrent metadata refreshes can
    // identify a personal workspace. Never publish from the older local hint.
    seat_require_team_identity(&current)?;
    Ok(evidence)
}

fn seat_live_quota_evidence(
    plan: Option<&str>,
    raw: Option<&Value>,
    now: i64,
) -> Result<(seat::QuotaEvidence, Option<i64>), seat::QuotaReadError> {
    match plan.map(str::trim).filter(|plan| !plan.is_empty()) {
        Some(plan) if plan.eq_ignore_ascii_case("free") => {
            Err("实时套餐已不具备工作区权益".into())
        }
        Some(_) => Ok(raw.map_or((seat::QuotaEvidence::Unknown, None), |raw| {
            seat::quota_evidence(raw, now)
        })),
        None => Ok((seat::QuotaEvidence::Unknown, None)),
    }
}

fn seat_quota_preparation_error(
    original: &CodexAccount,
    current: Option<&CodexAccount>,
    error: &str,
    language: &str,
) -> seat::QuotaReadError {
    let eligible = current.is_some_and(|current| {
        current.account_id == original.account_id
            && current.user_id == original.user_id
            && !current.is_api_key_auth()
            && !current.is_agent_identity_auth()
            && seat_require_team_identity(current).is_ok()
            && native_eligible(current)
    });
    // Token Authority wraps its local OAuth diagnostic in the selected locale.
    // Unwrap that exact template rather than searching arbitrary error bodies.
    let template = crate::modules::i18n::translate(
        language,
        "codex.authError.refreshFailed",
        &[("error", "\0")],
    );
    let diagnostic = template
        .split_once('\0')
        .and_then(|(prefix, suffix)| error.strip_prefix(prefix)?.strip_suffix(suffix))
        .unwrap_or(error);
    let temporary = diagnostic.starts_with("Token 刷新请求失败: ")
        || diagnostic.starts_with("读取响应失败: ")
        || diagnostic.starts_with("采集 Codex 额度运行态失败: ")
        || diagnostic
            .strip_prefix("Token 刷新失败: status=")
            .and_then(|tail| tail.split(|ch: char| !ch.is_ascii_digit()).next())
            .and_then(|status| status.parse::<u16>().ok())
            .is_some_and(|status| matches!(status, 408 | 429 | 500..=599));
    if eligible && temporary {
        seat::QuotaReadError::temporary("成员授权准备暂时失败，稍后重试")
    } else {
        "成员授权不可用或身份已变化，请重新核对".into()
    }
}

async fn seat_refresh_member_access(
    config: &seat::Config,
    id: &str,
    observed: u64,
) -> Result<CodexAccount, String> {
    let original = seat_member_account(config, id)?;
    let refreshed = codex_account::force_refresh_managed_account_after_observed(
        id,
        observed,
        "premium-seat-member-token-expired",
    )
    .await
    .map_err(|_| format!("成员 {} 的凭据已过期且刷新失败，请重新授权", original.email))?;
    if refreshed.account_id != original.account_id || refreshed.user_id != original.user_id {
        return Err("刷新后成员身份发生变化，已停止席位验证".into());
    }
    Ok(refreshed)
}

async fn seat_recover_pending_members(
    config: &seat::Config,
    state: &seat::State,
) -> Result<(), String> {
    if state.paused || !state.pending() {
        return Ok(());
    }
    let op = state.operation.as_ref().ok_or("席位操作缺失")?;
    let collection = seat_collection(config).await?;
    let key = collection
        .api_keys
        .iter()
        .find(|key| key.id == config.api_key_id && key.enabled)
        .ok_or("API Key 已改变")?;
    let pool = native_scoped_account_ids(&collection, Some(key));
    for (id, user) in [
        (&op.source_account_id, &op.source_user_id),
        (&op.target_account_id, &op.target_user_id),
    ] {
        if !pool.contains(id)
            || (!config.use_api_key_pool && !config.member_account_ids.contains(id))
        {
            return Err("来源或目标成员已离开授权账号池，不能刷新或恢复验证".into());
        }
        let account = seat_member_account(config, id)?;
        if account.user_id.as_deref() != Some(user.as_str()) {
            return Err("操作期间成员身份已改变，不能恢复验证".into());
        }
        if account.quota_error.as_ref().is_some_and(|error| {
            error.code.as_deref() == Some("token_expired")
                && codex_account::account_has_remote_api_auth_rejection(&account)
        }) {
            seat_require_team_identity(&account)?;
            let refreshed =
                seat_refresh_member_access(config, id, account.token_generation).await?;
            let refreshed = seat_confirm_team_profile(refreshed).await?;
            let started = Instant::now();
            let result = timeout(
                Duration::from_secs(30),
                codex_quota::fetch_quota(&refreshed),
            )
            .await
            .map_err(|_| "恢复成员授权后的额度核对超时")?
            .map_err(|_| "成员凭据已刷新，但额度核对仍失败；请重新授权")?;
            let current = codex_account::save_refreshed_quota(
                &refreshed,
                result.quota,
                result.plan_type,
                started,
            )
            .map_err(|_| "恢复授权期间成员身份发生变化，请重新核对")?;
            seat_require_team_identity(&current)?;
        }
    }
    Ok(())
}

async fn seat_business_probe(config: &seat::Config, id: &str) -> Result<(), String> {
    let collection = seat_collection(config).await?;
    if !seat_candidate_ids(config, &collection)?
        .iter()
        .any(|candidate| candidate == id)
    {
        return Err("探测目标已离开有效账号池".into());
    }
    let key = collection
        .api_keys
        .iter()
        .find(|k| k.id == config.api_key_id)
        .ok_or("API Key 已改变")?;
    let account = seat_member_account(config, id)?;
    let model = seat_probe_model(config, &collection, key, &account)?;
    let body = serde_json::to_vec(&json!({"model":model,"input":"Reply OK.","max_output_tokens":128,"stream":false,"store":false}))
        .map_err(|_| "无法创建验证请求")?;
    let response = send_internal_api_service_request(
        id,
        "/v1/responses",
        &HashMap::new(),
        &body,
        Duration::from_secs(45),
    )
    .await
    .map_err(|_| "目标成员定向探测连接失败")?;
    if !response.status().is_success() {
        return Err(format!(
            "目标成员定向探测失败（HTTP {}）",
            response.status().as_u16()
        ));
    }
    let result: Value = response
        .json()
        .await
        .map_err(|_| "目标成员探测响应不完整")?;
    if result.get("status").and_then(Value::as_str) != Some("completed")
        || result.get("error").is_some_and(|v| !v.is_null())
        || result
            .get("output")
            .and_then(Value::as_array)
            .is_none_or(|o| o.is_empty())
    {
        return Err("目标成员探测没有返回完整成功响应".into());
    }
    Ok(())
}

/// Preview never changes a seat or publishes routing, even when supplied enabled=true.
pub async fn check_premium_seat_rotation(config: seat::Config) -> Result<seat::State, String> {
    config.validate()?;
    seat_confirm_team_member(&config, &config.admin_account_id).await?;
    let collection = seat_collection(&config).await?;
    let ids = seat_candidate_ids(&config, &collection)?;
    if ids.len() < 2 {
        return Err("当前 Key 的有效池内至少需要两个同工作区、具备模型权限的 OAuth 成员".into());
    }
    let (members, _) = seat_read_snapshot(&config).await?;
    let remote_eligible = ids
        .iter()
        .filter(|id| {
            seat_member_account(&config, id)
                .ok()
                .is_some_and(|account| {
                    members.iter().any(|member| {
                        Some(member.id.as_str()) == account.user_id.as_deref() && member.eligible()
                    })
                })
        })
        .count();
    if remote_eligible < 2 {
        return Err("账号池内至少需要两个远端席位状态正常的同工作区成员".into());
    }
    let holder = seat_local_holder(&config, &collection, &members, &ids)?;
    let mut state = get_premium_seat_rotation()?;
    state.holder_account_id = Some(holder);
    state.message =
        Some("只读核对通过；未调换席位，未发布路由。启用后仍需额度与定向探测验证。".into());
    Ok(state)
}

pub async fn configure_premium_seat_rotation(config: seat::Config) -> Result<seat::State, String> {
    config.validate()?;
    if config.enabled {
        check_premium_seat_rotation(config.clone()).await?;
    }
    let dir = seat_dir()?;
    let _lock = seat::coordinator_lock(&dir)?;
    let mut state = seat::load(&dir)?;
    if state.pending() {
        return Err("存在未核对的席位操作，请先暂停并重新核对；不能覆盖操作记录".into());
    }
    if state
        .config
        .as_ref()
        .is_some_and(|old| old.workspace_id != config.workspace_id)
    {
        state.exhausted_until.clear();
    }
    state.config = Some(config);
    state.revision += 1;
    state.phase = seat::Phase::Idle;
    state.holder_account_id = None;
    state.holder_user_id = None;
    state.validated_at = None;
    state.paused = false;
    state.message = None;
    state.next_check_at = None;
    // Keep the last operation and exhausted windows for audit/deduplication.
    seat::save(&dir, &state)?;
    if seat_pause_path(&dir).exists() {
        fs::remove_file(seat_pause_path(&dir)).map_err(|_| "无法清除暂停标记")?;
    }
    ensure_premium_seat_monitor_started();
    Ok(state)
}

pub async fn control_premium_seat_rotation(action: &str) -> Result<seat::State, String> {
    let dir = seat_dir()?;
    if action == "next" {
        let _lock = seat::coordinator_lock(&dir)?;
        let mut state = seat::load(&dir)?;
        let config = state.config.clone().ok_or("请先保存席位调换配置")?;
        seat::worker::check_manual_next(&dir, &config, &state, seat_now())?;
        seat_collection(&config).await?;
        let backend = PremiumSeatBackend {
            config: &config,
            api: tokio::sync::Mutex::new(None),
            refresh_admin: true,
        };
        let result = seat::worker::switch_next(&dir, &config, &mut state, &backend).await;
        if seat_pause_path(&dir).exists() {
            state.paused = true;
        }
        if let Err(error) = seat::save(&dir, &state) {
            let _ = write_string_atomic(&seat_pause_path(&dir), "state_write_failed");
            return Err(error);
        }
        result?;
        ensure_premium_seat_monitor_started();
        return Ok(state);
    }
    if action == "pause" {
        // Separate persistent stop flag can be set while a worker owns its lock.
        if fs::read_to_string(seat_pause_path(&dir)).ok().as_deref() != Some("state_write_failed") {
            write_string_atomic(&seat_pause_path(&dir), "paused")?;
        }
        if let Ok(_lock) = seat::coordinator_lock(&dir) {
            let mut state = seat::load(&dir)?;
            state.paused = true;
            state.message = Some("已暂停新的席位调换；已提交操作仍会核对结果".into());
            seat::save(&dir, &state)?;
            return Ok(state);
        }
        let mut state = seat::load(&dir)?;
        state.paused = true;
        state.message = Some("已请求暂停；已提交的调换无法撤销，后台将核对实际结果".into());
        return Ok(state);
    }
    if !matches!(action, "resume" | "resolve_unapplied") {
        return Err("未知席位控制操作".into());
    }
    let _lock = seat::coordinator_lock(&dir)?;
    let mut state = seat::load(&dir)?;
    if action == "resolve_unapplied" {
        let config = state.config.as_ref().ok_or("尚未配置席位调换")?;
        let op = state
            .operation
            .as_ref()
            .filter(|_| state.pending())
            .ok_or("没有未决操作")?;
        if op
            .submitted_at
            .is_some_and(|at| seat_now() - at < seat::VERIFY_TIMEOUT_SECS)
        {
            return Err(format!(
                "提交后至少等待 {} 秒再确认未执行结果",
                seat::VERIFY_TIMEOUT_SECS
            ));
        }
        let (members, counts) = seat_read_snapshot(config).await?;
        if seat::reconcile(op, &members, &counts) != seat::Pair::Before {
            return Err("远端并非原席位分配，不能标记为未执行".into());
        }
        state
            .operation
            .as_mut()
            .ok_or("操作缺失")?
            .resolved_without_swap = true;
        state.attention("已核对远端仍为原分配，并确认结束未执行操作；可修改配置或手动恢复");
        seat::save(&dir, &state)?;
        return Ok(state);
    }
    let config = state.config.clone().ok_or("尚未配置席位调换")?;
    let resume_check = if config.enabled {
        check_premium_seat_rotation(config.clone())
            .await
            .map(|_| ())
    } else {
        Err("请先保存并启用席位调换配置".into())
    };
    if let Err(error) = resume_check {
        state.attention(error.clone());
        if state.pending() {
            // Losing eligibility must not strand an already-submitted operation.
            // A paused step may read its remote result, never probe or swap again.
            if let Err(reconcile_error) = premium_seat_step(&dir, &config, &mut state).await {
                state.attention(format!("{error}；未决操作只读核对失败：{reconcile_error}"));
            }
            seat::save(&dir, &state)?;
            ensure_premium_seat_monitor_started();
            return Ok(state);
        }
        seat::save(&dir, &state)?;
        return Err(error);
    }
    state.paused = false;
    state.revision += 1;
    state.message = None;
    state.validated_at = None;
    state.next_check_at = None;
    if let Some(op) = state
        .operation
        .as_mut()
        .filter(|o| o.verified_at.is_none() && !o.resolved_without_swap)
    {
        // Explicit resume authorizes one fresh probe, but NEVER another swap POST.
        op.probe_started = false;
    } else {
        state.holder_account_id = None;
        state.holder_user_id = None;
        state.phase = seat::Phase::Idle;
    }
    seat::save(&dir, &state)?;
    if seat_pause_path(&dir).exists() {
        fs::remove_file(seat_pause_path(&dir)).map_err(|_| "无法恢复席位任务")?;
    }
    ensure_premium_seat_monitor_started();
    Ok(state)
}

fn ensure_premium_seat_monitor_started() {
    if SEAT_MONITOR_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(seat::MONITOR_TICK_SECS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            premium_seat_tick().await;
        }
    });
}

async fn premium_seat_tick() {
    let Ok(dir) = seat_dir() else {
        return;
    };
    let Ok(_lock) = seat::coordinator_lock(&dir) else {
        return;
    };
    let Ok(mut state) = seat::load(&dir) else {
        return;
    };
    let Some(config) = state
        .config
        .clone()
        .filter(|c| c.enabled || state.pending())
    else {
        return;
    };
    if seat_pause_path(&dir).exists() {
        state.paused = true;
    }
    if state.paused
        && !state.pending()
        && !seat::worker::can_recheck_paused_holder(&state)
        && !seat::worker::can_retry_monitor_read(&dir, &state)
    {
        return;
    }
    if state.next_check_at.is_some_and(|at| at > seat_now()) {
        return;
    }
    let previous_status = (state.phase.clone(), state.paused, state.message.clone());
    let result = premium_seat_step(&dir, &config, &mut state).await;
    if let Err(error) = result {
        state.attention(error);
    }
    if seat_pause_path(&dir).exists() {
        state.paused = true;
    }
    if previous_status != (state.phase.clone(), state.paused, state.message.clone()) {
        let diagnostic = format!(
            "高级席位任务状态变化: phase={:?}, paused={}, message={}",
            state.phase,
            state.paused,
            state.message.as_deref().unwrap_or("-")
        );
        if state.paused {
            logger::log_warn(&diagnostic);
        } else {
            logger::log_info(&diagnostic);
        }
    }
    if let Err(error) = seat::save(&dir, &state) {
        // Do not keep an earlier publication alive after a failed state commit.
        match write_string_atomic(&seat_pause_path(&dir), "state_write_failed") {
            Ok(()) => logger::log_warn(&format!(
                "高级席位任务状态写入失败，已设置暂停标记: {error}"
            )),
            Err(pause_error) => logger::log_warn(&format!(
                "高级席位任务状态与暂停标记均写入失败: state_error={error}, pause_error={pause_error}"
            )),
        }
    }
}

struct PremiumSeatBackend<'a> {
    config: &'a seat::Config,
    api: tokio::sync::Mutex<Option<seat::http::AdminApi>>,
    refresh_admin: bool,
}

impl seat::worker::Backend for PremiumSeatBackend<'_> {
    fn now(&self) -> i64 {
        seat_now()
    }
    fn member(&self, id: &str) -> Result<String, String> {
        seat_member_account(self.config, id)?
            .user_id
            .ok_or_else(|| "成员身份缺失".into())
    }
    async fn holder(&self, members: &[seat::Member]) -> Result<String, String> {
        let collection = seat_collection(self.config).await?;
        let ids = seat_candidate_ids(self.config, &collection)?;
        seat_local_holder(self.config, &collection, members, &ids)
    }
    async fn candidates(&self) -> Result<Vec<String>, String> {
        let collection = seat_collection(self.config).await?;
        seat_candidate_ids(self.config, &collection)
    }
    async fn check_scope(&self) -> Result<(), String> {
        seat_confirm_team_member(self.config, &self.config.admin_account_id).await?;
        let collection = seat_collection(self.config).await?;
        seat_candidate_ids(self.config, &collection).map(|_| ())
    }
    async fn check_holder_scope(&self, id: &str) -> Result<(), String> {
        let collection = seat_collection(self.config).await?;
        seat_check_holder_scope(self.config, &collection, id)
    }
    async fn snapshot(&self) -> Result<seat::http::Snapshot, seat::http::AdminError> {
        let mut guard = self.api.lock().await;
        if guard.is_none() {
            // Prepare inside the typed read path so temporary preparation
            // failures receive the same retry policy as member queries.
            *guard = Some(seat_admin_api(self.config).await?);
        }
        let api = guard.as_mut().ok_or("管理员网络连接缺失")?;
        if self.refresh_admin {
            seat_admin_snapshot(self.config, api).await
        } else {
            // Failed authorization and pending operations do not refresh again
            // on every paused tick. Explicit resume permits another attempt.
            api.snapshot().await
        }
    }
    async fn quota(
        &self,
        id: &str,
    ) -> Result<(seat::QuotaEvidence, Option<i64>), seat::QuotaReadError> {
        seat_fresh_quota(self.config, id).await
    }
    async fn probe(&self, id: &str) -> Result<(), String> {
        seat_business_probe(self.config, id).await
    }
    async fn swap(&self, op: &seat::Operation) -> Result<(), String> {
        // Re-read the local identities at the final mutation boundary too.
        seat_team_member_account(self.config, &self.config.admin_account_id)?;
        seat_team_member_account(self.config, &op.source_account_id)?;
        seat_team_member_account(self.config, &op.target_account_id)?;
        self.api
            .lock()
            .await
            .as_ref()
            .ok_or("管理员网络连接缺失")?
            .swap(op)
            .await
    }
}

async fn premium_seat_step(
    dir: &Path,
    config: &seat::Config,
    state: &mut seat::State,
) -> Result<(), String> {
    if let Some(op) = state.operation.as_ref().filter(|_| state.pending()) {
        let team_scope = [
            config.admin_account_id.as_str(),
            op.source_account_id.as_str(),
            op.target_account_id.as_str(),
        ]
        .iter()
        .try_for_each(|id| seat_team_member_account(config, id).map(|_| ()));
        if let Err(error) = team_scope {
            // Preserve read-only reconciliation, but do not refresh/probe members
            // or publish routing after their team eligibility has disappeared.
            state.attention(error);
        }
    }
    seat_recover_pending_members(config, state).await?;
    let backend = PremiumSeatBackend {
        config,
        api: tokio::sync::Mutex::new(None),
        refresh_admin: seat::worker::can_refresh_admin(state)
            || seat::worker::can_retry_monitor_read(dir, state),
    };
    seat::worker::step(dir, config, state, &backend).await
}

fn seat_route_allows(
    state: &seat::State,
    key: Option<&str>,
    account: &str,
    internal: bool,
) -> bool {
    state.allows(key, account, internal, seat_now())
}

fn seat_validate_prepared_route(
    account: &CodexAccount,
    key: Option<&str>,
    internal: bool,
) -> Result<(), native_contract::ServiceError> {
    let (_, state) = seat_routing_state()?;
    let managed = !internal
        && state
            .config
            .as_ref()
            .is_some_and(|c| c.enabled && key == Some(c.api_key_id.as_str()));
    if !seat_route_allows(&state, key, &account.id, internal)
        || (managed
            && (account.is_api_key_auth()
                || account.is_agent_identity_auth()
                || seat_require_team_identity(account).is_err()
                || !native_eligible(account)
                || state.holder_user_id != account.user_id
                || state.config.as_ref().map(|c| c.workspace_id.as_str())
                    != account.account_id.as_deref()))
    {
        return Err(native_error(
            503,
            "premium_seat_not_verified",
            "Seat holder or authorization changed during route preparation",
        ));
    }
    // The administrator is needed to verify or change seats, not to use an
    // already verified holder. Its credentials must not gate normal requests.
    Ok(())
}

#[cfg(test)]
#[path = "codex_local_access_seat_route_tests.rs"]
mod seat_route_tests;

fn seat_routing_state() -> Result<(PathBuf, seat::State), native_contract::ServiceError> {
    let dir = seat_dir().map_err(|_| {
        native_error(
            503,
            "seat_state_unavailable",
            "Seat coordination storage is unavailable",
        )
    })?;
    if fs::read_to_string(seat_pause_path(&dir)).ok().as_deref() == Some("state_write_failed") {
        return Err(native_error(
            503,
            "seat_state_unavailable",
            "Seat state commit failed; reconcile before resuming",
        ));
    }
    let state = seat::load(&dir).map_err(|_| {
        native_error(
            503,
            "seat_state_unavailable",
            "Seat coordination state is invalid; reconcile before routing",
        )
    })?;
    Ok((dir, state))
}

fn seat_native_lease(
    account: &CodexAccount,
    key: Option<&str>,
    internal: bool,
    permit: Option<Arc<dyn Send + Sync>>,
) -> Result<Option<Arc<dyn Send + Sync>>, native_contract::ServiceError> {
    if account.is_api_key_auth()
        || account.is_agent_identity_auth()
        || native_is_chatgpt_plan_account(account)
    {
        return Ok(permit);
    }
    let (dir, _) = seat_routing_state()?;
    let lease = seat::admit(&dir, &account.id, permit).map_err(|_| {
        native_error(
            503,
            "seat_rotation_in_progress",
            "Account is draining for a seat change",
        )
    })?;
    let (_, state) = seat_routing_state()?;
    if !seat_route_allows(&state, key, &account.id, internal) {
        return Err(native_error(
            503,
            "premium_seat_not_verified",
            "No verified premium holder is available for this API key",
        ));
    }
    Ok(Some(lease))
}
