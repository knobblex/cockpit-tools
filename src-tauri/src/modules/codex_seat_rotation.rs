//! Persistent, fail-closed policy for one existing ChatGPT premium seat.
//! The worker and native gateway share this store and OS advisory locks.
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
};

pub const VERIFY_TIMEOUT_SECS: i64 = 180;
pub const MIN_SWAP_INTERVAL_SECS: i64 = 300;
pub const RAPID_EXHAUSTION_WINDOW_SECS: i64 = 30 * 60;
pub const MONITOR_CHECK_SECS: i64 = 60;
pub const MONITOR_RETRY_SECS: i64 = 30;
pub const MONITOR_TICK_SECS: u64 = 30;
pub const DRAIN_TIMEOUT_SECS: u64 = 60;

/// Keep temporary quota reads distinct from authorization/identity failures.
/// Only sanitized diagnostics cross into the persisted seat state.
#[derive(Debug)]
pub struct QuotaReadError {
    retryable: bool,
    message: String,
}

impl QuotaReadError {
    pub fn temporary(message: &str) -> Self {
        Self {
            retryable: true,
            message: message.into(),
        }
    }

    pub fn retryable(&self) -> bool {
        self.retryable
    }

    pub fn from_fetch(error: &str) -> Self {
        // Inspect only diagnostics emitted by fetch_quota, never response bodies.
        if error.starts_with("请求失败: ") || error.starts_with("读取响应失败: ") {
            return Self::temporary("实时额度查询连接失败");
        }
        let status = error
            .strip_prefix("API 返回错误 ")
            .and_then(|tail| tail.split_whitespace().next())
            .and_then(|status| status.parse::<u16>().ok());
        if let Some(status) = status {
            return Self {
                retryable: matches!(status, 408 | 429 | 500..=599),
                message: format!("实时额度查询失败（HTTP {status}）；未触发调换"),
            };
        }
        "实时额度查询失败；未触发调换".into()
    }
}

impl From<&str> for QuotaReadError {
    fn from(message: &str) -> Self {
        Self {
            retryable: false,
            message: message.into(),
        }
    }
}

impl From<String> for QuotaReadError {
    fn from(message: String) -> Self {
        Self {
            retryable: false,
            message,
        }
    }
}

impl From<QuotaReadError> for String {
    fn from(error: QuotaReadError) -> Self {
        error.message
    }
}

#[path = "codex_seat_rotation_http.rs"]
pub mod http;
#[path = "codex_seat_rotation_worker.rs"]
pub mod worker;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub workspace_id: String,
    pub admin_account_id: String,
    pub api_key_id: String,
    /// Missing on the first version: retain its explicitly authorized member list.
    #[serde(default)]
    pub use_api_key_pool: bool,
    pub member_account_ids: Vec<String>,
    /// Empty selects from the target member's existing authorized model catalog.
    #[serde(default)]
    pub probe_model: String,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if uuid::Uuid::parse_str(&self.workspace_id).is_err()
            || self.admin_account_id.trim().is_empty()
            || self.api_key_id.trim().is_empty()
            || (!self.use_api_key_pool && self.member_account_ids.len() < 2)
            || self.member_account_ids.len() > 50
            || self
                .member_account_ids
                .iter()
                .any(|id| id.trim().is_empty())
            || self
                .member_account_ids
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.member_account_ids.len()
        {
            return Err(
                "席位配置无效：需要工作区、管理员与 API Key；旧版名单至少需要两个不同成员".into(),
            );
        }
        Ok(())
    }
}

/// Discovery does not send trial requests. Manual overrides never fall back to
/// another model; the adapter must enforce the same ACL for either path.
pub(crate) fn select_probe_model<T: AsRef<str>>(
    requested: &str,
    discovered: impl IntoIterator<Item = T>,
    mut authorized: impl FnMut(&str) -> Option<String>,
) -> Result<String, String> {
    let requested = requested.trim();
    if !requested.is_empty() {
        return authorized(requested)
            .ok_or_else(|| "指定探测模型不可用或不在此成员的授权范围内".into());
    }
    discovered
        .into_iter()
        .map(|model| model.as_ref().trim().to_owned())
        .filter(|model| !model.is_empty())
        .find_map(|model| authorized(&model))
        .ok_or_else(|| {
            "无法从此 Key 与目标成员的模型目录中发现可用探测模型；请核对模型权限或手动指定".into()
        })
}

pub(crate) fn expired_member_access_token(error: &str) -> bool {
    error.contains("API 返回错误 401") && error.contains("[error_code:token_expired]")
}

pub(crate) fn has_personal_structure(account_structure: Option<&str>) -> bool {
    let structure = account_structure
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    structure.contains("个人")
        || structure
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|word| matches!(word, "personal" | "individual"))
}

/// A local team identity is necessary, never sufficient: the adapter must also
/// verify this workspace's live administrator, members and premium-seat support.
/// Seat-specific quota plans (for example `prolite`) do not erase an explicit
/// workspace structure, but an explicitly personal structure always wins.
pub(crate) fn has_team_identity(plan_type: Option<&str>, account_structure: Option<&str>) -> bool {
    if has_personal_structure(account_structure) {
        return false;
    }
    let structure = account_structure
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let structure_words = structure
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .collect::<Vec<_>>();
    if structure_words.iter().any(|word| {
        matches!(
            *word,
            "team" | "workspace" | "business" | "enterprise" | "organization" | "edu"
        )
    }) {
        return true;
    }
    plan_type
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|word| matches!(word, "team" | "business" | "enterprise" | "edu"))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Ready,
    Draining,
    Submitted,
    Verifying,
    NeedsAttention,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operation {
    pub id: String,
    pub workspace_id: String,
    pub source_account_id: String,
    pub target_account_id: String,
    pub source_user_id: String,
    pub target_user_id: String,
    pub before_counts: BTreeMap<String, u64>,
    pub created_at: i64,
    /// Written BEFORE POST. A restart must reconcile, never resend this attempt.
    pub submitted_at: Option<i64>,
    /// None on older records: conservatively count a completed swap as exhaustion-triggered.
    #[serde(default)]
    pub exhaustion_triggered: Option<bool>,
    /// Written BEFORE the business probe; a crash cannot silently repeat it.
    pub probe_started: bool,
    pub verified_at: Option<i64>,
    pub resolved_without_swap: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct State {
    pub version: u32,
    pub revision: u64,
    pub config: Option<Config>,
    pub paused: bool,
    pub phase: Phase,
    pub holder_account_id: Option<String>,
    pub holder_user_id: Option<String>,
    /// Last complete holder verification, retained while quota is exhausted.
    /// Routing depends on Ready and local authorization, not a validation lease.
    pub validated_at: Option<i64>,
    pub operation: Option<Operation>,
    pub history: Vec<Operation>,
    /// Exhausted holders cannot immediately receive the same seat again.
    pub exhausted_until: BTreeMap<String, i64>,
    pub message: Option<String>,
    pub next_check_at: Option<i64>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            config: None,
            paused: false,
            phase: Phase::Idle,
            holder_account_id: None,
            holder_user_id: None,
            validated_at: None,
            operation: None,
            history: Vec::new(),
            exhausted_until: BTreeMap::new(),
            message: None,
            next_check_at: None,
        }
    }
}

impl State {
    pub fn pending(&self) -> bool {
        self.operation
            .as_ref()
            .is_some_and(|op| op.verified_at.is_none() && !op.resolved_without_swap)
    }
    pub fn attention(&mut self, message: impl Into<String>) {
        self.paused = true;
        self.phase = Phase::NeedsAttention;
        self.validated_at = None;
        self.message = Some(message.into());
    }
    pub fn cancel_prepared(&mut self) {
        if let Some(op) = self
            .operation
            .as_mut()
            .filter(|op| op.submitted_at.is_none())
        {
            op.resolved_without_swap = true;
        }
        self.phase = Phase::Idle;
        self.validated_at = None;
    }
    pub fn routing_unverified(&self, key: Option<&str>, internal: bool, now: i64) -> bool {
        !internal
            && self
                .config
                .as_ref()
                .is_some_and(|config| config.enabled && key == Some(config.api_key_id.as_str()))
            && !self
                .holder_account_id
                .as_deref()
                .is_some_and(|holder| self.allows(key, holder, false, now))
    }

    pub fn allows(&self, key: Option<&str>, account: &str, internal: bool, now: i64) -> bool {
        // A mutation affects the whole workspace, including other local API keys.
        if let Some(op) = self.operation.as_ref().filter(|_| self.pending()) {
            if account == op.source_account_id {
                return false;
            }
            if account == op.target_account_id && !internal {
                return false;
            }
        }
        let Some(config) = self.config.as_ref().filter(|c| c.enabled) else {
            return true;
        };
        if internal || key != Some(config.api_key_id.as_str()) {
            return true;
        }
        self.phase == Phase::Ready
            && self.holder_account_id.as_deref() == Some(account)
            && self
                .validated_at
                .is_some_and(|at| at <= now)
    }
}

pub fn load(dir: &Path) -> Result<State, String> {
    let path = dir.join("codex_premium_seat_rotation.json");
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(_) => return Err("无法读取席位状态；已阻止路由".into()),
    };
    let state: State = serde_json::from_slice(&raw).map_err(|_| "席位状态损坏；已阻止路由")?;
    if state.version != 1 {
        return Err("不支持的席位状态版本".into());
    }
    if let Some(config) = &state.config {
        config.validate()?;
    }
    if state.pending()
        && state.config.as_ref().is_none_or(|config| {
            state
                .operation
                .as_ref()
                .is_none_or(|op| op.workspace_id != config.workspace_id)
        })
    {
        return Err("未决操作与工作区配置不一致".into());
    }
    if state.phase == Phase::Ready
        && (state.holder_user_id.as_deref().is_none_or(str::is_empty)
            || state.validated_at.is_none()
            || state.config.as_ref().is_none_or(|c| {
                if c.use_api_key_pool {
                    state.holder_account_id.as_deref().is_none_or(str::is_empty)
                } else {
                    !c.member_account_ids
                        .iter()
                        .any(|id| Some(id) == state.holder_account_id.as_ref())
                }
            }))
    {
        return Err("持席成员的验证状态不完整".into());
    }
    if let Some(op) = &state.operation {
        if op.source_account_id == op.target_account_id
            || op.source_user_id == op.target_user_id
            || op.created_at <= 0
            || uuid::Uuid::parse_str(&op.id).is_err()
            || op
                .verified_at
                .is_some_and(|_| !op.probe_started || op.resolved_without_swap)
            || op.submitted_at.is_some_and(|at| at < op.created_at)
        {
            return Err("席位操作记录无效".into());
        }
    }
    Ok(state)
}

pub fn save(dir: &Path, state: &State) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(state).map_err(|_| "无法序列化席位状态")?;
    crate::modules::atomic_write::write_string_atomic(
        &dir.join("codex_premium_seat_rotation.json"),
        &raw,
    )
}

fn open_lock(path: PathBuf) -> Result<File, String> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| "无法打开席位协调锁".into())
}

/// A single coordinator per data directory. Holding the file across awaits is intentional.
pub fn coordinator_lock(dir: &Path) -> Result<File, String> {
    let file = open_lock(dir.join("codex_premium_seat_rotation.lock"))?;
    file.try_lock_exclusive()
        .map_err(|_| "席位任务正在执行，请稍后重试")?;
    Ok(file)
}

fn account_lock(dir: &Path, account: &str) -> Result<File, String> {
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(account.as_bytes()));
    open_lock(dir.join(format!("codex_seat_account_{digest}.lock")))
}

/// Also drains requests from other local gateway processes sharing this data directory.
pub fn drain_lock(dir: &Path, account: &str) -> Result<File, String> {
    let file = account_lock(dir, account)?;
    file.try_lock_exclusive()
        .map_err(|_| "正在等待账号的在途请求结束")?;
    Ok(file)
}

struct RequestLease {
    _file: File,
    _upstream_permit: Option<Arc<dyn Send + Sync>>,
}

pub fn admit(
    dir: &Path,
    account: &str,
    permit: Option<Arc<dyn Send + Sync>>,
) -> Result<Arc<dyn Send + Sync>, String> {
    let file = account_lock(dir, account)?;
    FileExt::try_lock_shared(&file).map_err(|_| "账号正在调换席位")?;
    // Recheck after acquiring the shared lease: the worker persists Draining before
    // trying the exclusive lease, closing the selection/admission race.
    let state = load(dir)?;
    if state
        .operation
        .as_ref()
        .is_some_and(|op| state.pending() && op.source_account_id == account)
    {
        return Err("账号正在调换席位".into());
    }
    Ok(Arc::new(RequestLease {
        _file: file,
        _upstream_permit: permit,
    }))
}

#[derive(Clone, Debug, Deserialize)]
pub struct Member {
    pub id: String,
    #[serde(default)]
    pub role: String,
    pub seat_type: String,
    #[serde(default)]
    pub pending_seat_type: Option<String>,
    #[serde(default)]
    pub is_trial: bool,
    #[serde(default)]
    pub deactivated_time: Option<Value>,
}

impl Member {
    pub fn eligible(&self) -> bool {
        !self.is_trial
            && self.deactivated_time.as_ref().is_none_or(Value::is_null)
            && self.pending_seat_type.is_none()
            && matches!(self.seat_type.as_str(), "default" | "prolite")
    }
}

/// Count the full key pool before model/authorization eligibility filtering, so
/// an unusable second premium member cannot silently disappear from the policy.
pub(crate) fn pool_premium_holder<'a>(
    members: &'a [Member],
    pool_user_ids: &[String],
) -> Result<&'a Member, String> {
    let premium = members
        .iter()
        .filter(|member| member.seat_type == "prolite" && pool_user_ids.contains(&member.id))
        .collect::<Vec<_>>();
    if premium.len() != 1 {
        return Err("所选 API Key 的账号池内必须恰好有一个高级席位；只能调换池内成员的席位".into());
    }
    if !premium[0].eligible() {
        return Err("账号池内的高级席位成员状态不满足自动调换条件".into());
    }
    Ok(premium[0])
}

pub fn swap_body(op: &Operation) -> Value {
    // Verified against the ChatGPT members UI bundle on 2026-10-20.
    // seat_type describes the CURRENT seat, not the desired seat.
    serde_json::json!({
        "source": {"seat_type":"prolite", "user_id":op.source_user_id},
        "target": {"seat_type":"default", "user_id":op.target_user_id},
        "mutation_attempt_id": op.id,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pair {
    Before,
    After,
    Conflict,
}

pub fn reconcile(op: &Operation, members: &[Member], counts: &BTreeMap<String, u64>) -> Pair {
    if counts != &op.before_counts {
        return Pair::Conflict;
    }
    let source = members
        .iter()
        .filter(|m| m.id == op.source_user_id)
        .collect::<Vec<_>>();
    let target = members
        .iter()
        .filter(|m| m.id == op.target_user_id)
        .collect::<Vec<_>>();
    if source.len() != 1 || target.len() != 1 || !source[0].eligible() || !target[0].eligible() {
        return Pair::Conflict;
    }
    match (source[0].seat_type.as_str(), target[0].seat_type.as_str()) {
        ("prolite", "default") => Pair::Before,
        ("default", "prolite") => Pair::After,
        _ => Pair::Conflict,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum QuotaEvidence {
    Available,
    Exhausted,
    Unknown,
}

/// Use the unrounded subscription windows, never a generic HTTP 429 or UI percent.
pub fn quota_evidence(raw: &Value, now: i64) -> (QuotaEvidence, Option<i64>) {
    let Some(rate) = raw.get("rate_limit").filter(|v| v.is_object()) else {
        return (QuotaEvidence::Unknown, None);
    };
    let mut windows = 0;
    let mut exhausted = false;
    let mut reset = None;
    for name in ["primary_window", "secondary_window"] {
        let Some(w) = rate.get(name).filter(|v| !v.is_null()) else {
            continue;
        };
        let (Some(used), Some(length), Some(at)) = (
            w.get("used_percent").and_then(Value::as_f64),
            w.get("limit_window_seconds").and_then(Value::as_i64),
            w.get("reset_at").and_then(Value::as_i64),
        ) else {
            return (QuotaEvidence::Unknown, None);
        };
        if !used.is_finite() || !(0.0..=100.0).contains(&used) || length <= 0 || at <= now {
            return (QuotaEvidence::Unknown, None);
        }
        windows += 1;
        if used >= 100.0 {
            exhausted = true;
            reset = Some(reset.map_or(at, |old: i64| old.max(at)));
        }
    }
    if windows == 0 {
        return (QuotaEvidence::Unknown, None);
    }
    // Percentages may already read 100 while the upstream still permits usage.
    // An explicit allowance wins only after validating all subscription windows.
    if rate.get("allowed").and_then(Value::as_bool) == Some(true)
        && rate.get("limit_reached").and_then(Value::as_bool) == Some(false)
    {
        return (QuotaEvidence::Available, None);
    }
    if exhausted {
        return if rate.get("allowed").and_then(Value::as_bool) == Some(false)
            && rate.get("limit_reached").and_then(Value::as_bool) == Some(true)
        {
            (QuotaEvidence::Exhausted, reset)
        } else {
            (QuotaEvidence::Unknown, None)
        };
    }
    (QuotaEvidence::Unknown, None)
}

#[cfg(test)]
#[path = "codex_seat_rotation_tests.rs"]
mod tests;
