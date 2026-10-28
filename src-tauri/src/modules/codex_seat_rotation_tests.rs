use super::worker::Backend as _;
use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    Mutex,
};

fn config() -> Config {
    Config {
        enabled: true,
        workspace_id: "00000000-0000-4000-8000-000000000001".into(),
        admin_account_id: "a".into(),
        api_key_id: "key".into(),
        use_api_key_pool: false,
        member_account_ids: vec!["a".into(), "b".into()],
        probe_model: "gpt-test".into(),
    }
}

#[test]
fn probe_model_is_optional_in_saved_and_incoming_config() {
    let mut automatic = config();
    automatic.probe_model.clear();
    automatic.validate().unwrap();
    let mut value = serde_json::to_value(&automatic).unwrap();
    value.as_object_mut().unwrap().remove("probeModel");
    let decoded: Config = serde_json::from_value(value).unwrap();
    assert!(decoded.probe_model.is_empty());
    decoded.validate().unwrap();
    automatic.api_key_id.clear();
    assert!(automatic.validate().is_err());
}

#[test]
fn automatic_probe_selection_skips_denied_models_and_preserves_catalog_order() {
    let catalog = ["", "denied", "prefix/allowed", "second"];
    let mut inspected = Vec::new();
    let selected = select_probe_model("  ", catalog, |name| {
        inspected.push(name.to_owned());
        match name {
            "prefix/allowed" => Some("upstream-allowed".to_owned()),
            "second" => Some("upstream-second".to_owned()),
            _ => None,
        }
    })
    .unwrap();
    assert_eq!(selected, "upstream-allowed");
    assert_eq!(inspected, ["denied", "prefix/allowed"]);
}

#[test]
fn manual_probe_override_never_falls_back_to_discovered_models() {
    let mut inspected = Vec::new();
    assert!(select_probe_model(" forbidden ", ["allowed"], |name| {
        inspected.push(name.to_owned());
        (name == "allowed").then(|| name.to_owned())
    })
    .is_err());
    assert_eq!(inspected, ["forbidden"]);
    assert_eq!(
        select_probe_model(" prefix/manual ", ["other"], |name| {
            (name == "prefix/manual").then(|| "canonical".to_owned())
        })
        .unwrap(),
        "canonical"
    );
}

#[test]
fn automatic_probe_selection_fails_closed_without_an_authorized_model() {
    assert!(select_probe_model("", ["denied"], |_| None).is_err());
    assert!(select_probe_model("", Vec::<String>::new(), |_| Some("guessed".into())).is_err());
}

#[test]
fn member_access_refresh_requires_a_401_with_the_exact_expired_access_code() {
    assert!(expired_member_access_token(
        "API 返回错误 401 Unauthorized [error_code:token_expired]"
    ));
    for error in [
        "API 返回错误 403 [error_code:token_expired]",
        "API 返回错误 401 [error_code:refresh_token_expired]",
        "API 返回错误 401 [error_code:token_revoked]",
        "API 返回错误 429 [error_code:token_expired]",
    ] {
        assert!(!expired_member_access_token(error));
    }
}

#[test]
fn team_identity_matches_the_shared_frontend_boundary_cases() {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        name: String,
        plan_type: Option<String>,
        account_structure: Option<String>,
        eligible: bool,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!(
        "../../../tests/codex-seat-rotation/team-eligibility-cases.json"
    ))
    .unwrap();
    for case in cases {
        assert_eq!(
            has_team_identity(case.plan_type.as_deref(), case.account_structure.as_deref()),
            case.eligible,
            "{}",
            case.name
        );
    }
}

#[test]
fn confirmed_workspace_structure_survives_seat_plan_changes() {
    assert!(has_team_identity(Some("team"), None));
    assert!(!has_team_identity(None, None)); // Requires the real profile first.
    assert!(has_team_identity(None, Some("workspace")));
    assert!(has_team_identity(Some("prolite"), Some("workspace")));
    assert!(has_team_identity(Some("pro"), Some("workspace")));
    for remote_structure in [None, Some("unknown"), Some("personal"), Some("个人")] {
        assert!(!has_team_identity(None, remote_structure));
    }
    assert!(!has_team_identity(Some("team"), Some("personal")));
}

fn state() -> State {
    State {
        config: Some(config()),
        ..State::default()
    }
}
fn members(after: bool) -> Vec<Member> {
    [
        ("ua", if after { "default" } else { "prolite" }),
        ("ub", if after { "prolite" } else { "default" }),
    ]
    .into_iter()
    .map(|(id, seat_type)| Member {
        id: id.into(),
        role: "account-owner".into(),
        seat_type: seat_type.into(),
        pending_seat_type: None,
        is_trial: false,
        deactivated_time: None,
    })
    .collect()
}
fn counts() -> BTreeMap<String, u64> {
    [("default".into(), 1), ("prolite".into(), 1)]
        .into_iter()
        .collect()
}

#[test]
fn premium_holder_is_unique_in_the_full_pool_and_ignores_outside_seats() {
    let mut remote = members(false);
    let mut outside = remote[0].clone();
    outside.id = "outside".into();
    remote.push(outside);
    let pool = vec!["ua".into(), "ub".into()];
    assert_eq!(pool_premium_holder(&remote, &pool).unwrap().id, "ua");
    assert!(pool_premium_holder(&remote, &["ub".into()]).is_err());
    assert!(pool_premium_holder(&remote, &[]).is_err());
    remote[1].seat_type = "prolite".into();
    // Even an ineligible second premium member counts toward the pool limit.
    remote[1].is_trial = true;
    assert!(pool_premium_holder(&remote, &pool).is_err());
    remote[1].seat_type = "default".into();
    remote[0].pending_seat_type = Some("default".into());
    assert!(pool_premium_holder(&remote, &pool).is_err());
}

struct Mock {
    dir: PathBuf,
    now: AtomicI64,
    after: AtomicBool,
    posts: AtomicUsize,
    probes: Mutex<Vec<String>>,
    target_available: AtomicBool,
    lost_post_response: AtomicBool,
    lost_snapshot_once: AtomicBool,
    snapshot_error: Mutex<Option<http::AdminError>>,
    snapshot_error_on_read: AtomicUsize,
    permission: AtomicBool,
    team_identity: AtomicBool,
    lose_team_on_check: AtomicUsize,
    scope_checks: AtomicUsize,
    snapshot_reads: AtomicUsize,
    revoke_on_check: AtomicUsize,
    conflicting_counts: AtomicBool,
    outside_premium: AtomicBool,
    expand_pool_on_read: AtomicUsize,
    quota_error: AtomicBool,
    rejected_holder_credential: AtomicBool,
    temporary_quota_error: AtomicBool,
    source_available: AtomicBool,
    target_quota_error: AtomicBool,
    probe_error: AtomicBool,
    candidate_error: AtomicBool,
    pool: Mutex<Vec<String>>,
    target: Mutex<String>,
    remove_target_on_final_check: AtomicBool,
    exhaust_holder_on_recheck: AtomicBool,
    holder_quota_reads: AtomicUsize,
    holder_quota_unknown: AtomicBool,
}
impl Mock {
    fn new(dir: &Path) -> Self {
        Self {
            dir: dir.into(),
            now: AtomicI64::new(1000),
            after: AtomicBool::new(false),
            posts: AtomicUsize::new(0),
            probes: Mutex::new(vec![]),
            target_available: AtomicBool::new(true),
            lost_post_response: AtomicBool::new(false),
            lost_snapshot_once: AtomicBool::new(false),
            snapshot_error: Mutex::new(None),
            snapshot_error_on_read: AtomicUsize::new(1),
            permission: AtomicBool::new(true),
            team_identity: AtomicBool::new(true),
            lose_team_on_check: AtomicUsize::new(usize::MAX),
            scope_checks: AtomicUsize::new(0),
            snapshot_reads: AtomicUsize::new(0),
            revoke_on_check: AtomicUsize::new(usize::MAX),
            conflicting_counts: AtomicBool::new(false),
            outside_premium: AtomicBool::new(false),
            expand_pool_on_read: AtomicUsize::new(usize::MAX),
            quota_error: AtomicBool::new(false),
            rejected_holder_credential: AtomicBool::new(false),
            temporary_quota_error: AtomicBool::new(false),
            source_available: AtomicBool::new(false),
            target_quota_error: AtomicBool::new(false),
            probe_error: AtomicBool::new(false),
            candidate_error: AtomicBool::new(false),
            pool: Mutex::new(vec!["a".into(), "b".into()]),
            target: Mutex::new("b".into()),
            remove_target_on_final_check: AtomicBool::new(false),
            exhaust_holder_on_recheck: AtomicBool::new(false),
            holder_quota_reads: AtomicUsize::new(0),
            holder_quota_unknown: AtomicBool::new(false),
        }
    }
}
impl worker::Backend for Mock {
    fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
    fn member(&self, id: &str) -> Result<String, String> {
        match id {
            "a" => Ok("ua".into()),
            "b" => Ok("ub".into()),
            "c" => Ok("uc".into()),
            _ => Err("unknown member".into()),
        }
    }
    async fn holder(&self, members: &[Member]) -> Result<String, String> {
        let pool = self.pool.lock().unwrap();
        let users = pool
            .iter()
            .map(|id| self.member(id))
            .collect::<Result<Vec<_>, _>>()?;
        let holder = pool_premium_holder(members, &users)?;
        pool.iter()
            .find(|id| self.member(id).ok().as_deref() == Some(holder.id.as_str()))
            .cloned()
            .ok_or_else(|| "holder outside pool".into())
    }
    async fn candidates(&self) -> Result<Vec<String>, String> {
        if self.candidate_error.load(Ordering::SeqCst) {
            return Err("unrelated legacy candidate is no longer eligible".into());
        }
        Ok(self.pool.lock().unwrap().clone())
    }
    async fn check_holder_scope(&self, account: &str) -> Result<(), String> {
        self.member(account)?;
        if !self.team_identity.load(Ordering::SeqCst) {
            return Err("holder team identity unconfirmed".into());
        }
        if !self.pool.lock().unwrap().iter().any(|id| id == account) {
            return Err("holder outside current key pool".into());
        }
        Ok(())
    }
    async fn check_scope(&self) -> Result<(), String> {
        let check = self.scope_checks.fetch_add(1, Ordering::SeqCst) + 1;
        if !self.team_identity.load(Ordering::SeqCst)
            || check >= self.lose_team_on_check.load(Ordering::SeqCst)
        {
            return Err("team identity unconfirmed".into());
        }
        if check == 3 && self.remove_target_on_final_check.load(Ordering::SeqCst) {
            self.pool.lock().unwrap().retain(|id| id != "b");
        }
        if self.permission.load(Ordering::SeqCst)
            && check < self.revoke_on_check.load(Ordering::SeqCst)
        {
            Ok(())
        } else {
            Err("ACL revoked".into())
        }
    }
    async fn snapshot(&self) -> Result<http::Snapshot, http::AdminError> {
        let read = self.snapshot_reads.fetch_add(1, Ordering::SeqCst) + 1;
        if read == self.expand_pool_on_read.load(Ordering::SeqCst) {
            self.pool.lock().unwrap().push("c".into());
        }
        if read >= self.snapshot_error_on_read.load(Ordering::SeqCst) {
            if let Some(error) = self.snapshot_error.lock().unwrap().take() {
                return Err(error);
            }
        }
        if self.after.load(Ordering::SeqCst)
            && self.lost_snapshot_once.swap(false, Ordering::SeqCst)
        {
            return Err(http::AdminError::connection("成员查询连接失败"));
        }
        let after = self.after.load(Ordering::SeqCst);
        let mut remote_counts = counts();
        let include_third = self.pool.lock().unwrap().iter().any(|id| id == "c")
            && !self.outside_premium.load(Ordering::SeqCst);
        if include_third {
            remote_counts.insert("default".into(), 2);
        }
        if after && self.conflicting_counts.load(Ordering::SeqCst) {
            remote_counts.insert("prolite".into(), 2);
        }
        let mut remote_members = members(false);
        if include_third {
            remote_members.push(Member {
                id: "uc".into(),
                role: "account-owner".into(),
                seat_type: "default".into(),
                pending_seat_type: None,
                is_trial: false,
                deactivated_time: None,
            });
        }
        if after {
            let target = format!("u{}", self.target.lock().unwrap());
            for member in &mut remote_members {
                member.seat_type = if member.id == target {
                    "prolite"
                } else {
                    "default"
                }
                .into();
            }
        }
        if self.outside_premium.load(Ordering::SeqCst) {
            let mut outside = members(false).remove(0);
            outside.id = "uc".into();
            remote_members.push(outside);
            *remote_counts.entry("prolite".into()).or_default() += 1;
        }
        Ok((remote_members, remote_counts))
    }
    async fn quota(&self, id: &str) -> Result<(QuotaEvidence, Option<i64>), QuotaReadError> {
        if self.rejected_holder_credential.load(Ordering::SeqCst) {
            return Err(QuotaReadError::from_fetch("API 返回错误 401 Unauthorized"));
        }
        if self.temporary_quota_error.load(Ordering::SeqCst) {
            return Err(QuotaReadError::temporary("实时额度查询超时"));
        }
        if id == "a" && self.holder_quota_unknown.load(Ordering::SeqCst) {
            return Ok((QuotaEvidence::Unknown, None));
        }
        let holder = if self.after.load(Ordering::SeqCst) {
            self.target.lock().unwrap().clone()
        } else {
            "a".into()
        };
        if self.exhaust_holder_on_recheck.load(Ordering::SeqCst)
            && id == holder
            && self.holder_quota_reads.fetch_add(1, Ordering::SeqCst) > 0
        {
            return Ok((QuotaEvidence::Exhausted, Some(9000)));
        }
        if self.quota_error.load(Ordering::SeqCst)
            || (id == "b" && self.target_quota_error.load(Ordering::SeqCst))
        {
            return Err("HTTP 429 frequency limit".into());
        }
        if (id == "a" && self.source_available.load(Ordering::SeqCst))
            || (id == *self.target.lock().unwrap()
                && self.after.load(Ordering::SeqCst)
                && self.target_available.load(Ordering::SeqCst))
        {
            Ok((QuotaEvidence::Available, None))
        } else {
            Ok((QuotaEvidence::Exhausted, Some(9000)))
        }
    }
    async fn probe(&self, id: &str) -> Result<(), String> {
        let stored = load(&self.dir)?;
        if stored.pending() {
            assert!(stored.operation.as_ref().unwrap().probe_started);
        }
        assert!(!stored.allows(Some("key"), id, false, self.now()));
        self.probes.lock().unwrap().push(id.into());
        if self.probe_error.load(Ordering::SeqCst) {
            Err("probe failed".into())
        } else {
            Ok(())
        }
    }
    async fn swap(&self, op: &Operation) -> Result<(), String> {
        let stored = load(&self.dir)?;
        assert_eq!(stored.phase, Phase::Submitted);
        assert_eq!(stored.operation.as_ref().unwrap().id, op.id);
        assert!(stored.operation.as_ref().unwrap().submitted_at.is_some());
        assert!(admit(&self.dir, &op.source_account_id, None).is_err());
        assert!(drain_lock(&self.dir, &op.source_account_id).is_err());
        assert!(!stored.allows(Some("other-key"), &op.source_account_id, false, self.now()));
        self.posts.fetch_add(1, Ordering::SeqCst);
        *self.target.lock().unwrap() = op.target_account_id.clone();
        self.after.store(true, Ordering::SeqCst);
        if self.lost_post_response.load(Ordering::SeqCst) {
            Err("POST timeout".into())
        } else {
            Ok(())
        }
    }
}

async fn run(mock: &Mock, state: &mut State) -> Result<(), String> {
    let current_config = state.config.clone().unwrap();
    let result = worker::step(&mock.dir, &current_config, state, mock).await;
    if let Err(error) = &result {
        state.attention(error.clone());
    }
    save(&mock.dir, state).unwrap();
    result
}

fn ready_state() -> State {
    State {
        phase: Phase::Ready,
        holder_account_id: Some("a".into()),
        holder_user_id: Some("ua".into()),
        validated_at: Some(1000),
        ..state()
    }
}

#[tokio::test]
async fn rotation_changes_only_pool_members_when_workspace_has_other_premium_seats() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.outside_premium.store(true, Ordering::SeqCst);
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(state.holder_account_id.as_deref(), Some("b"));
    let op = state.operation.as_ref().unwrap();
    assert_eq!((&*op.source_user_id, &*op.target_user_id), ("ua", "ub"));
    assert_eq!(op.before_counts["prolite"], 2);
    let (remote, counts) = mock.snapshot().await.unwrap();
    assert_eq!(counts["prolite"], 2);
    assert_eq!(
        remote.iter().find(|m| m.id == "uc").unwrap().seat_type,
        "prolite"
    );
}

#[tokio::test]
async fn a_second_pool_premium_seat_blocks_start_commit_and_publication() {
    for read in [1, 2, 4] {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(dir.path());
        mock.outside_premium.store(true, Ordering::SeqCst);
        mock.expand_pool_on_read.store(read, Ordering::SeqCst);
        let mut state = state();
        assert!(run(&mock, &mut state).await.is_err());
        assert_eq!(mock.posts.load(Ordering::SeqCst), usize::from(read == 4));
        assert!(!state.allows(Some("key"), "b", false, 1000));
    }
}

#[tokio::test]
async fn pending_recovery_rejects_a_second_premium_member_added_to_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.outside_premium.store(true, Ordering::SeqCst);
    mock.lost_snapshot_once.store(true, Ordering::SeqCst);
    let mut state = state();
    assert!(run(&mock, &mut state).await.is_err());
    assert!(state.pending());
    mock.pool.lock().unwrap().push("c".into());
    state.paused = false;
    assert!(run(&mock, &mut state).await.is_err());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(mock.probes.lock().unwrap().is_empty());
    assert!(!state.allows(Some("key"), "b", false, 1000));
}

#[tokio::test]
async fn manual_next_moves_a_healthy_seat_and_rejects_immediate_repeat() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    let config = state.config.clone().unwrap();
    worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(state.holder_account_id.as_deref(), Some("b"));
    assert_eq!(*mock.probes.lock().unwrap(), vec!["b"]);
    assert!(!state.exhausted_until.contains_key("a"));
    assert!(state.allows(Some("key"), "b", false, 1000));
    assert!(worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .is_err());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn manual_next_uses_circular_order_even_for_a_legacy_member_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    *mock.pool.lock().unwrap() = vec!["b".into(), "a".into(), "c".into()];
    let mut state = ready_state();
    let config = state.config.clone().unwrap();
    worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .unwrap();
    assert_eq!(state.holder_account_id.as_deref(), Some("c"));
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn manual_next_with_no_candidate_keeps_the_healthy_current_route() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    mock.target_quota_error.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    let config = state.config.clone().unwrap();
    worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(!state.pending());
    assert!(state.allows(Some("key"), "a", false, 1000));
    assert!(!state.exhausted_until.contains_key("a"));
}

#[tokio::test]
async fn manual_next_detects_an_administrator_change_and_revokes_the_old_route() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.after.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    let config = state.config.clone().unwrap();
    assert!(worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .is_err());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(state.paused);
    assert!(!state.allows(Some("key"), "a", false, 1000));
}

#[tokio::test]
async fn manual_next_cannot_bypass_team_eligibility() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.team_identity.store(false, Ordering::SeqCst);
    let mut state = ready_state();
    let config = state.config.clone().unwrap();
    assert!(worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .is_err());
    assert!(state.paused);
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 0);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(!state.allows(Some("key"), "a", false, 1000));
}

#[tokio::test]
async fn manual_next_rejects_disabled_paused_or_unverified_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    for condition in ["disabled", "paused", "unverified", "pause_file"] {
        let mut state = ready_state();
        match condition {
            "disabled" => state.config.as_mut().unwrap().enabled = false,
            "paused" => state.paused = true,
            "unverified" => state.phase = Phase::Idle,
            "pause_file" => std::fs::write(worker::pause_path(dir.path()), "paused").unwrap(),
            _ => unreachable!(),
        }
        let config = state.config.clone().unwrap();
        assert!(
            worker::switch_next(dir.path(), &config, &mut state, &mock)
                .await
                .is_err(),
            "{condition}"
        );
        assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn manual_next_accepts_old_validation_but_rechecks_before_swapping() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    mock.now.store(1000 + 30 * 24 * 60 * 60, Ordering::SeqCst);
    let mut state = ready_state();
    let config = state.config.clone().unwrap();
    worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(mock.snapshot_reads.load(Ordering::SeqCst) >= 3);
    assert_eq!(*mock.probes.lock().unwrap(), ["b"]);
    assert!(state.allows(Some("key"), "b", false, mock.now()));
}

#[tokio::test]
async fn manual_next_probe_failure_blocks_publication_and_another_manual_swap() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    mock.probe_error.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    let config = state.config.clone().unwrap();
    assert!(worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .is_err());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    let persisted = load(dir.path()).unwrap();
    assert!(persisted.pending());
    assert!(!persisted.allows(Some("key"), "b", false, 1000));
    assert!(worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .is_err());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn exhausted_holder_swaps_only_once_and_publishes_after_target_probe() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(*mock.probes.lock().unwrap(), vec!["b"]);
    assert_eq!(state.holder_account_id.as_deref(), Some("b"));
    assert!(state.allows(Some("key"), "b", false, 1000));
    assert!(!state.allows(Some("key"), "a", false, 1000));
    assert_eq!(state.exhausted_until.get("a"), Some(&9000));
    run(&mock, &mut state).await.unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(mock.probes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn rapid_exhaustion_pauses_after_one_swap_and_stays_paused_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.pool.lock().unwrap().push("c".into());
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    let operation_id = state.operation.as_ref().unwrap().id.clone();
    assert_eq!(
        state.operation.as_ref().unwrap().exhaustion_triggered,
        Some(true)
    );

    mock.target_available.store(false, Ordering::SeqCst);
    mock.now
        .store(1000 + MIN_SWAP_INTERVAL_SECS, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    assert!(state.paused);
    assert_eq!(state.phase, Phase::NeedsAttention);
    assert!(state.message.as_deref().unwrap().contains("避免循环"));
    assert!(state.next_check_at.is_none());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(!state.allows(Some("key"), "b", false, 1300));
    assert_eq!(state.operation.as_ref().unwrap().id, operation_id);

    let mut restarted = load(dir.path()).unwrap();
    assert!(restarted.paused);
    mock.now
        .store(1000 + RAPID_EXHAUSTION_WINDOW_SECS + 1, Ordering::SeqCst);
    run(&mock, &mut restarted).await.unwrap();
    assert!(restarted.paused);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn rapid_exhaustion_history_blocks_manual_next_and_an_early_resume() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.pool.lock().unwrap().push("c".into());
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    // Older completed operations still protect against a later healthy manual swap.
    state.history.push(state.operation.take().unwrap());
    mock.target_available.store(false, Ordering::SeqCst);
    mock.now
        .store(1000 + MIN_SWAP_INTERVAL_SECS, Ordering::SeqCst);
    state.validated_at = Some(1300);
    let config = state.config.clone().unwrap();
    worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .unwrap();
    save(dir.path(), &state).unwrap();
    assert!(state.paused);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);

    let mut restarted = load(dir.path()).unwrap();
    restarted.paused = false; // Explicit resume does not erase completed operations.
    run(&mock, &mut restarted).await.unwrap();
    assert!(restarted.paused);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);

    restarted.paused = false;
    mock.now
        .store(1000 + RAPID_EXHAUSTION_WINDOW_SECS, Ordering::SeqCst);
    run(&mock, &mut restarted).await.unwrap();
    assert!(!restarted.paused);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 2);
    assert_eq!(restarted.operation.as_ref().unwrap().target_account_id, "c");
}

#[tokio::test]
async fn a_healthy_manual_swap_does_not_arm_the_exhaustion_guard() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.pool.lock().unwrap().push("c".into());
    mock.source_available.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    let config = state.config.clone().unwrap();
    worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .unwrap();
    assert_eq!(
        state.operation.as_ref().unwrap().exhaustion_triggered,
        Some(false)
    );
    mock.target_available.store(false, Ordering::SeqCst);
    mock.now
        .store(1000 + MIN_SWAP_INTERVAL_SECS, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    assert!(!state.paused);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn manual_preparation_that_becomes_exhausted_stops_before_post() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.pool.lock().unwrap().push("c".into());
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    mock.now
        .store(1000 + MIN_SWAP_INTERVAL_SECS, Ordering::SeqCst);
    state.validated_at = Some(1300);
    mock.exhaust_holder_on_recheck.store(true, Ordering::SeqCst);
    let config = state.config.clone().unwrap();
    worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .unwrap();
    assert!(state.paused);
    assert_eq!(state.phase, Phase::NeedsAttention);
    assert!(!state.pending());
    let canceled = state.operation.as_ref().unwrap();
    assert!(canceled.submitted_at.is_none());
    assert!(canceled.resolved_without_swap);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn older_operations_without_a_trigger_are_guarded_conservatively() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    let mut raw = serde_json::to_value(&state).unwrap();
    raw["operation"]
        .as_object_mut()
        .unwrap()
        .remove("exhaustionTriggered");
    std::fs::write(
        dir.path().join("codex_premium_seat_rotation.json"),
        raw.to_string(),
    )
    .unwrap();
    let mut restarted = load(dir.path()).unwrap();
    assert_eq!(
        restarted.operation.as_ref().unwrap().exhaustion_triggered,
        None
    );
    mock.target_available.store(false, Ordering::SeqCst);
    run(&mock, &mut restarted).await.unwrap();
    assert!(restarted.paused);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lost_post_response_reconciles_success_without_resending() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.lost_post_response.store(true, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    assert_eq!(state.phase, Phase::Ready);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn restart_after_uncertain_write_recovers_by_reading_remote_pair() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.lost_snapshot_once.store(true, Ordering::SeqCst);
    assert!(worker::step(dir.path(), &config(), &mut state, &mock)
        .await
        .is_err());
    // Simulate process termination: discard unsaved in-memory changes.
    let mut recovered = load(dir.path()).unwrap();
    assert_eq!(recovered.phase, Phase::Submitted);
    run(&mock, &mut recovered).await.unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(recovered.phase, Phase::Ready);
}

#[tokio::test]
async fn submitted_but_unapplied_attempt_is_never_retried() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.lost_snapshot_once.store(true, Ordering::SeqCst);
    let _ = worker::step(dir.path(), &config(), &mut state, &mock).await;
    mock.after.store(false, Ordering::SeqCst);
    let mut recovered = load(dir.path()).unwrap();
    run(&mock, &mut recovered).await.unwrap();
    assert!(recovered.paused);
    assert!(!recovered.allows(Some("key"), "a", false, 1000));
    run(&mock, &mut recovered).await.unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn target_zero_balance_pauses_without_looping_back_to_old_holder() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.target_available.store(false, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    assert_eq!(state.phase, Phase::Verifying);
    assert!(!state.allows(Some("key"), "b", false, 1000));
    mock.now.store(1200, Ordering::SeqCst);
    assert!(run(&mock, &mut state).await.is_err());
    assert!(state.paused);
    run(&mock, &mut state).await.unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(mock.probes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn probe_failure_and_restart_do_not_repeat_probe_or_publish() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.probe_error.store(true, Ordering::SeqCst);
    assert!(run(&mock, &mut state).await.is_err());
    let mut recovered = load(dir.path()).unwrap();
    recovered.paused = false; // Crash recovery without explicit resetting of probe_started.
    assert!(run(&mock, &mut recovered).await.is_err());
    assert_eq!(mock.probes.lock().unwrap().len(), 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(!recovered.allows(Some("key"), "b", false, 1000));
}

#[tokio::test]
async fn frequency_limit_or_revoked_key_never_triggers_seat_write() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.quota_error.store(true, Ordering::SeqCst);
    assert!(run(&mock, &mut state()).await.is_err());
    mock.quota_error.store(false, Ordering::SeqCst);
    mock.permission.store(false, Ordering::SeqCst);
    assert!(run(&mock, &mut state()).await.is_err());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unconfirmed_team_identity_blocks_before_any_remote_read_or_write() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.team_identity.store(false, Ordering::SeqCst);
    assert!(run(&mock, &mut state).await.is_err());
    assert!(state.paused);
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 0);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(mock.probes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn team_identity_lost_before_final_submission_never_swaps() {
    for check in [2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(dir.path());
        let mut state = state();
        mock.lose_team_on_check.store(check, Ordering::SeqCst);
        assert!(run(&mock, &mut state).await.is_err());
        assert!(state.paused);
        assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
        assert!(state.operation.as_ref().unwrap().submitted_at.is_none());
        assert!(!state.allows(Some("key"), "a", false, mock.now()));
    }
}

#[tokio::test]
async fn submitted_operation_remains_readable_without_team_eligibility() {
    for applied in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(dir.path());
        let mut state = state();
        mock.lost_snapshot_once.store(true, Ordering::SeqCst);
        assert!(worker::step(dir.path(), &config(), &mut state, &mock)
            .await
            .is_err());
        let mut recovered = load(dir.path()).unwrap();
        recovered.paused = true;
        mock.after.store(applied, Ordering::SeqCst);
        mock.team_identity.store(false, Ordering::SeqCst);
        mock.snapshot_reads.store(0, Ordering::SeqCst);
        run(&mock, &mut recovered).await.unwrap();
        assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 1);
        assert!(recovered.paused);
        assert!(recovered.pending());
        assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
        assert!(mock.probes.lock().unwrap().is_empty());
        assert!(!recovered.allows(Some("key"), "b", false, mock.now()));
    }
}

#[tokio::test]
async fn unpaused_pending_operation_cannot_probe_after_team_eligibility_loss() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.lost_snapshot_once.store(true, Ordering::SeqCst);
    assert!(worker::step(dir.path(), &config(), &mut state, &mock)
        .await
        .is_err());
    let mut recovered = load(dir.path()).unwrap();
    mock.team_identity.store(false, Ordering::SeqCst);
    mock.snapshot_reads.store(0, Ordering::SeqCst);
    assert!(run(&mock, &mut recovered).await.is_err());
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 1);
    assert!(recovered.paused);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(mock.probes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn disabled_pending_configuration_can_reconcile_without_resuming() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.lost_snapshot_once.store(true, Ordering::SeqCst);
    assert!(worker::step(dir.path(), &config(), &mut state, &mock)
        .await
        .is_err());
    let mut recovered = load(dir.path()).unwrap();
    recovered.config.as_mut().unwrap().enabled = false;
    recovered.paused = true;
    mock.team_identity.store(false, Ordering::SeqCst);
    mock.snapshot_reads.store(0, Ordering::SeqCst);
    run(&mock, &mut recovered).await.unwrap();
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 1);
    assert!(recovered.paused);
    assert!(recovered.pending());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(mock.probes.lock().unwrap().is_empty());
    assert!(!recovered.allows(Some("key"), "b", false, mock.now()));
}

#[tokio::test]
async fn published_holder_removed_from_eligible_pool_is_paused_without_another_swap() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    assert!(state.allows(Some("key"), "b", false, mock.now()));
    mock.pool.lock().unwrap().retain(|id| id != "b");
    assert!(run(&mock, &mut state).await.is_err());
    assert!(state.paused);
    assert!(!state.allows(Some("key"), "b", false, mock.now()));
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn changed_local_holder_identity_blocks_a_previously_verified_route() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    state.holder_user_id = Some("changed-user".into());
    assert!(run(&mock, &mut state).await.is_err());
    assert!(!state.allows(Some("key"), "a", false, mock.now()));
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 0);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rejected_holder_credentials_still_revoke_an_existing_route_without_swapping() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.rejected_holder_credential
        .store(true, Ordering::SeqCst);
    let mut state = ready_state();
    assert!(run(&mock, &mut state).await.is_err());
    assert!(state.paused);
    assert!(!state.allows(Some("key"), "a", false, mock.now()));
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 0);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pause_while_draining_cancels_before_post_and_keeps_stream_alive() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    let lease = admit(dir.path(), "a", None).unwrap();
    let config = config();
    let task = worker::step(dir.path(), &config, &mut state, &mock);
    let pause = async {
        loop {
            if load(dir.path()).unwrap().phase == Phase::Draining {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        std::fs::write(worker::pause_path(dir.path()), "paused").unwrap();
    };
    let (result, _) = tokio::join!(task, pause);
    result.unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(state.paused);
    assert!(drain_lock(dir.path(), "a").is_err());
    drop(lease);
    assert!(drain_lock(dir.path(), "a").is_ok());
}

#[tokio::test]
async fn no_candidates_preserves_seat_and_waits() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    state.exhausted_until.insert("b".into(), 9000);
    run(&mock, &mut state).await.unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(!state.pending());
    assert!(state.next_check_at.is_some());
}

#[tokio::test]
async fn unavailable_candidate_is_excluded_without_submitting_or_pausing_monitor() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.target_quota_error.store(true, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(!state.paused);
    assert!(state
        .message
        .as_deref()
        .unwrap()
        .contains("b: 授权或实时额度查询不可用"));
}

#[test]
fn circular_pass_starts_after_holder_wraps_and_never_visits_holder() {
    let ids = vec!["a".into(), "b".into(), "c".into()];
    assert_eq!(
        worker::circular_candidates(&ids, "b").unwrap(),
        vec!["c", "a"]
    );
    assert_eq!(
        worker::circular_candidates(&ids, "c").unwrap(),
        vec!["a", "b"]
    );
    assert!(worker::circular_candidates(&ids, "removed").is_err());
    assert!(worker::circular_candidates(&[], "a").is_err());
    assert!(worker::circular_candidates(&["a".into()], "a")
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn live_pool_can_use_new_member_and_skip_unavailable_neighbor() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    state.config.as_mut().unwrap().use_api_key_pool = true;
    state.config.as_mut().unwrap().member_account_ids.clear();
    *mock.pool.lock().unwrap() = vec!["a".into(), "b".into(), "c".into()];
    mock.target_quota_error.store(true, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    assert_eq!(state.holder_account_id.as_deref(), Some("c"));
    assert_eq!(*mock.probes.lock().unwrap(), vec!["c"]);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(load(dir.path())
        .unwrap()
        .allows(Some("key"), "c", false, 1000));
}

#[tokio::test]
async fn live_pool_uses_next_member_instead_of_restarting_at_pool_head() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    state.config.as_mut().unwrap().use_api_key_pool = true;
    *mock.pool.lock().unwrap() = vec!["b".into(), "a".into(), "c".into()];
    run(&mock, &mut state).await.unwrap();
    assert_eq!(state.holder_account_id.as_deref(), Some("c"));
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn removing_candidate_from_pool_before_submit_prevents_swap() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    state.config.as_mut().unwrap().use_api_key_pool = true;
    mock.remove_target_on_final_check
        .store(true, Ordering::SeqCst);
    assert!(run(&mock, &mut state).await.is_err());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(state.paused);
}

#[test]
fn old_config_retains_member_boundary_until_explicitly_saved_in_pool_mode() {
    let mut value = serde_json::to_value(config()).unwrap();
    value.as_object_mut().unwrap().remove("useApiKeyPool");
    let legacy: Config = serde_json::from_value(value).unwrap();
    assert!(!legacy.use_api_key_pool);
    assert_eq!(legacy.member_account_ids, vec!["a", "b"]);
    let mut pool_config = legacy;
    pool_config.use_api_key_pool = true;
    pool_config.member_account_ids.clear();
    pool_config.validate().unwrap();
}

#[tokio::test]
async fn key_revoked_during_final_quota_check_prevents_remote_write() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.revoke_on_check.store(3, Ordering::SeqCst);
    assert!(run(&mock, &mut state).await.is_err());
    assert!(state.paused);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(state.operation.as_ref().unwrap().submitted_at.is_none());
}

#[tokio::test]
async fn unexpected_capacity_change_blocks_probe_and_publication() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    mock.conflicting_counts.store(true, Ordering::SeqCst);
    assert!(run(&mock, &mut state).await.is_err());
    assert!(state.paused);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(mock.probes.lock().unwrap().is_empty());
    assert!(!state.allows(Some("key"), "b", false, 1000));
}

#[tokio::test]
async fn administrator_change_is_reconciled_before_a_subsequent_exhaustion_swap() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    mock.after.store(false, Ordering::SeqCst);
    mock.now
        .store(1000 + RAPID_EXHAUSTION_WINDOW_SECS + 1, Ordering::SeqCst);
    let snapshots = mock.snapshot_reads.load(Ordering::SeqCst);
    assert!(run(&mock, &mut state).await.is_err());
    assert!(!state.allows(Some("key"), "b", false, 1000));
    assert!(mock.snapshot_reads.load(Ordering::SeqCst) > snapshots);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
}

#[test]
fn leases_hold_drain_until_last_stream_clone_and_preserve_existing_permit() {
    struct Permit(Arc<AtomicBool>);
    impl Drop for Permit {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let lease = admit(dir.path(), "a", Some(Arc::new(Permit(dropped.clone())))).unwrap();
    let stream_clone = lease.clone();
    drop(lease);
    assert!(drain_lock(dir.path(), "a").is_err());
    assert!(!dropped.load(Ordering::SeqCst));
    drop(stream_clone);
    let exclusive = drain_lock(dir.path(), "a").unwrap();
    assert!(dropped.load(Ordering::SeqCst));
    assert!(admit(dir.path(), "a", None).is_err());
    drop(exclusive);
    assert!(admit(dir.path(), "a", None).is_ok());
}

#[test]
fn coordinator_lock_is_exclusive_and_crash_release_needs_no_file_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let first = coordinator_lock(dir.path()).unwrap();
    assert!(coordinator_lock(dir.path()).is_err());
    drop(first);
    assert!(coordinator_lock(dir.path()).is_ok());
}

#[test]
fn missing_corrupt_and_future_state_have_distinct_fail_closed_results() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load(dir.path())
        .unwrap()
        .allows(Some("any"), "a", false, 1000));
    std::fs::write(dir.path().join("codex_premium_seat_rotation.json"), "{").unwrap();
    assert!(load(dir.path()).is_err());
    let mut state = state();
    state.version = 2;
    save(dir.path(), &state).unwrap();
    assert!(load(dir.path()).is_err());
}

#[test]
fn publication_does_not_expire_and_cannot_override_key_scope_or_future_validation() {
    let mut state = state();
    state.phase = Phase::Ready;
    state.holder_account_id = Some("b".into());
    state.validated_at = Some(1000);
    assert!(state.allows(Some("key"), "b", false, 1180));
    assert!(state.allows(Some("key"), "b", false, 1000 + 30 * 24 * 60 * 60));
    assert!(!state.allows(Some("key"), "b", false, 999));
    assert!(state.allows(Some("other"), "a", false, 1000));
    assert!(!state.allows(Some("key"), "unrelated", false, 1000));
}

#[test]
fn rounded_percentage_missing_fields_and_generic_429_are_not_exhaustion() {
    let raw = serde_json::json!({"rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{
        "used_percent":99.999,"limit_window_seconds":18000,"reset_at":2000}}});
    assert_eq!(quota_evidence(&raw, 1000).0, QuotaEvidence::Available);
    let mut exhausted = raw.clone();
    exhausted["rate_limit"]["primary_window"]["used_percent"] = serde_json::json!(100);
    assert_eq!(quota_evidence(&exhausted, 1000).0, QuotaEvidence::Available);
    exhausted["rate_limit"]["allowed"] = serde_json::json!(false);
    exhausted["rate_limit"]["limit_reached"] = serde_json::json!(true);
    assert_eq!(
        quota_evidence(&exhausted, 1000),
        (QuotaEvidence::Exhausted, Some(2000))
    );
    assert_eq!(quota_evidence(&exhausted, 2000).0, QuotaEvidence::Unknown);
    assert_eq!(
        quota_evidence(&serde_json::json!({"status":429}), 1000).0,
        QuotaEvidence::Unknown
    );
    assert_eq!(
        quota_evidence(
            &serde_json::json!({"rate_limit":{"allowed":false,"limit_reached":true}}),
            1000
        )
        .0,
        QuotaEvidence::Unknown
    );
}

#[tokio::test]
async fn http_adapter_uses_observed_swap_wire_and_sanitizes_errors() {
    use axum::{
        routing::{get, post},
        Json,
    };
    let bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = bodies.clone();
    let app = axum::Router::new()
        .route(
            "/users",
            get(|| async {
                Json(serde_json::json!({"items":[
            {"id":"ua","seat_type":"prolite"},{"id":"ub","seat_type":"default"},
            {"id":"outside","seat_type":"prolite"}],"total":3}))
            }),
        )
        .route(
            "/users/seat_type_counts",
            get(|| async {
                Json(serde_json::json!({"seat_type_counts":{"prolite":2,"default":1,"unused":0}}))
            }),
        )
        .route(
            "/users/swap-seats",
            post(move |Json(body): Json<Value>| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(body);
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        "private-error-body-do-not-expose",
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let api = http::AdminApi::mock(
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .unwrap(),
        format!("http://{address}/users"),
    );
    let (remote, actual_counts) = api.snapshot().await.unwrap();
    assert_eq!(remote.len(), 3);
    assert_eq!(actual_counts["prolite"], 2);
    assert_eq!(actual_counts["default"], 1);
    let op = Operation {
        workspace_id: config().workspace_id,
        id: uuid::Uuid::new_v4().to_string(),
        source_account_id: "a".into(),
        target_account_id: "b".into(),
        source_user_id: "ua".into(),
        target_user_id: "ub".into(),
        before_counts: actual_counts,
        created_at: 1000,
        submitted_at: Some(1000),
        exhaustion_triggered: None,
        probe_started: false,
        verified_at: None,
        resolved_without_swap: false,
    };
    let error = api.swap(&op).await.unwrap_err();
    assert!(error.contains("403"));
    assert!(!error.contains("private-error"));
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0],
        serde_json::json!({"source":{"seat_type":"prolite","user_id":"ua"},
        "target":{"seat_type":"default","user_id":"ub"},"mutation_attempt_id":op.id})
    );
    server.abort();
}

#[tokio::test]
async fn admin_read_401_refreshes_once_and_retains_the_new_client() {
    use axum::{routing::get, Json};
    let reads = Arc::new(AtomicUsize::new(0));
    let captured = reads.clone();
    let app = axum::Router::new()
        .route("/expired/users", get(move || {
            captured.fetch_add(1, Ordering::SeqCst);
            async { axum::http::StatusCode::UNAUTHORIZED }
        }))
        .route("/fresh/users", get(|| async {
            Json(serde_json::json!({"items":[{"id":"ua","seat_type":"prolite"},{"id":"ub","seat_type":"default"}],"total":2}))
        }))
        .route("/fresh/users/seat_type_counts", get(|| async {
            Json(serde_json::json!({"seat_type_counts":{"prolite":1,"default":1}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut api = http::AdminApi::mock(client.clone(), format!("http://{address}/expired/users"));
    let refreshes = AtomicUsize::new(0);
    let (members, _) = api
        .snapshot_with_refresh(|generation| {
            assert_eq!(generation, 0);
            async {
                refreshes.fetch_add(1, Ordering::SeqCst);
                Ok(http::AdminApi::mock(
                    client.clone(),
                    format!("http://{address}/fresh/users"),
                ))
            }
        })
        .await
        .unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    api.snapshot_with_refresh(|_| async { panic!("refreshed client must be retained") })
        .await
        .unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn repeated_admin_401_is_bounded_and_does_not_expose_response_body() {
    use axum::routing::get;
    let reads = Arc::new(AtomicUsize::new(0));
    let captured = reads.clone();
    let app = axum::Router::new().route(
        "/users",
        get(move || {
            captured.fetch_add(1, Ordering::SeqCst);
            async {
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    "private-admin-token-detail",
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://{address}/users");
    let mut api = http::AdminApi::mock(client.clone(), base.clone());
    let refreshes = AtomicUsize::new(0);
    let error = api
        .snapshot_with_refresh(|_| async {
            refreshes.fetch_add(1, Ordering::SeqCst);
            Ok(http::AdminApi::mock(client.clone(), base.clone()))
        })
        .await
        .unwrap_err();
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert!(error.to_string().contains("刷新管理员凭据后"));
    assert!(error.to_string().contains("查询成员") && error.to_string().contains("401"));
    assert!(!error.to_string().contains("private-admin-token-detail"));
    server.abort();
}

#[tokio::test]
async fn admin_403_does_not_refresh_or_retry() {
    use axum::routing::get;
    let reads = Arc::new(AtomicUsize::new(0));
    let captured = reads.clone();
    let app = axum::Router::new().route(
        "/users",
        get(move || {
            captured.fetch_add(1, Ordering::SeqCst);
            async { axum::http::StatusCode::FORBIDDEN }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut api = http::AdminApi::mock(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{address}/users"),
    );
    let error = api
        .snapshot_with_refresh(|_| async { panic!("403 must not refresh credentials") })
        .await
        .unwrap_err();
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert!(error.to_string().contains("403") && error.to_string().contains("管理员角色"));
    server.abort();
}

#[tokio::test]
async fn paused_reconciliation_preserves_the_original_authentication_error() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.target_available.store(false, Ordering::SeqCst);
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    assert!(state.pending());
    state.attention("查询成员认证失败（HTTP 401）");
    run(&mock, &mut state).await.unwrap();
    assert_eq!(
        state.message.as_deref(),
        Some("查询成员认证失败（HTTP 401）")
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(mock.probes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn transient_quota_read_preserves_publication_without_revalidating_or_swapping() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    mock.temporary_quota_error.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    for now in [1060, 1120, 1181] {
        mock.now.store(now, Ordering::SeqCst);
        run(&mock, &mut state).await.unwrap();
        state = load(dir.path()).unwrap();
        assert!(!state.paused);
        assert_eq!(state.validated_at, Some(1000));
        assert_eq!(state.next_check_at, Some(now + MONITOR_RETRY_SECS));
        assert!(state.allows(Some("key"), "a", false, now));
        assert!(state.exhausted_until.is_empty());
    }
    mock.temporary_quota_error.store(false, Ordering::SeqCst);
    mock.now.store(1241, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    assert!(state.allows(Some("key"), "a", false, 1241));
    assert_eq!(state.validated_at, Some(1000));
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 0);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(mock.probes.lock().unwrap().is_empty());
}

#[test]
fn quota_read_errors_classify_only_local_diagnostics_and_hide_upstream_details() {
    for status in [400, 401, 403, 404, 408, 429, 500, 502, 503, 504, 599, 600] {
        let error = QuotaReadError::from_fetch(&format!(
            "API 返回错误 {status} [body:private-detail HTTP 503 请求失败: token_expired]"
        ));
        assert_eq!(error.retryable(), matches!(status, 408 | 429 | 500..=599));
        assert!(!String::from(error).contains("private-detail"));
    }
    for diagnostic in ["请求失败: private-detail", "读取响应失败: private-detail"] {
        let error = QuotaReadError::from_fetch(diagnostic);
        assert!(error.retryable());
        assert!(!String::from(error).contains("private-detail"));
    }
    for diagnostic in [
        "解析 JSON 失败: 请求失败: private-detail",
        "CODEX_QUOTA_IDENTITY_MISMATCH",
        "PROXY_BINDING_CHANGED",
        "unexpected [body:API 返回错误 503]",
        "API 返回错误 401 [body:请求失败: API 返回错误 503]",
    ] {
        assert!(!QuotaReadError::from_fetch(diagnostic).retryable());
    }
}

#[tokio::test]
async fn transient_quota_read_during_explicit_pause_never_resumes_rotations() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    state.paused = true;
    std::fs::write(worker::pause_path(dir.path()), "paused").unwrap();
    mock.temporary_quota_error.store(true, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    mock.now.store(1181, Ordering::SeqCst);
    assert!(state.allows(Some("key"), "a", false, 1181));
    mock.temporary_quota_error.store(false, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    assert!(state.paused);
    assert!(state.allows(Some("key"), "a", false, 1181));
    assert_eq!(
        state.message.as_deref(),
        Some("调换已暂停，当前高级成员已验证，正在监测实时订阅额度")
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn transient_quota_read_during_pending_verification_still_pauses_without_resubmitting() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.target_available.store(false, Ordering::SeqCst);
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    assert!(state.pending());
    mock.temporary_quota_error.store(true, Ordering::SeqCst);
    assert!(run(&mock, &mut state).await.is_err());
    assert!(state.paused);
    assert!(state.pending());
    assert!(!worker::can_retry_monitor_read(dir.path(), &state));
    assert!(!state.allows(Some("key"), "b", false, 1000));
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert!(mock.probes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn manual_next_does_not_queue_a_swap_after_a_temporary_quota_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.temporary_quota_error.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    let config = state.config.clone().unwrap();
    assert!(worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .is_err());
    assert!(state.paused);
    assert!(!state.pending());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn healthy_holder_ignores_admin_outages_without_snapshot_or_probe() {
    for paused in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(dir.path());
        mock.source_available.store(true, Ordering::SeqCst);
        mock.permission.store(false, Ordering::SeqCst);
        *mock.snapshot_error.lock().unwrap() =
            Some(http::AdminError::connection("成员查询连接失败"));
        let mut state = ready_state();
        state.paused = paused;
        if paused {
            std::fs::write(worker::pause_path(dir.path()), "paused").unwrap();
        }
        for now in [1060, 1181, 1000 + 30 * 24 * 60 * 60] {
            mock.now.store(now, Ordering::SeqCst);
            run(&mock, &mut state).await.unwrap();
            state = load(dir.path()).unwrap();
            assert_eq!(state.paused, paused);
            assert_eq!(state.validated_at, Some(1000));
            assert_eq!(state.next_check_at, Some(now + MONITOR_CHECK_SECS));
            assert!(state.allows(Some("key"), "a", false, now));
            assert!(!state.routing_unverified(Some("key"), false, now));
        }
        assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 0);
        assert_eq!(mock.scope_checks.load(Ordering::SeqCst), 0);
        assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
        assert!(mock.probes.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn healthy_legacy_holder_defers_unrelated_candidate_failure_until_switching() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    mock.candidate_error.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    assert!(!state.config.as_ref().unwrap().use_api_key_pool);
    for now in [1060, 1181, 1000 + 24 * 60 * 60] {
        mock.now.store(now, Ordering::SeqCst);
        run(&mock, &mut state).await.unwrap();
        assert!(state.allows(Some("key"), "a", false, now));
        assert_eq!(state.validated_at, Some(1000));
    }
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 0);
    assert_eq!(mock.scope_checks.load(Ordering::SeqCst), 0);
    assert!(mock.probes.lock().unwrap().is_empty());

    let config = state.config.clone().unwrap();
    assert!(worker::switch_next(dir.path(), &config, &mut state, &mock)
        .await
        .is_err());
    assert!(mock.snapshot_reads.load(Ordering::SeqCst) > 0);
    assert!(mock.scope_checks.load(Ordering::SeqCst) > 0);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn legacy_connection_pause_recovers_after_full_validation_without_a_swap() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    state.attention("成员查询连接失败");
    assert!(worker::can_retry_monitor_read(dir.path(), &state));
    *mock.snapshot_error.lock().unwrap() = Some(http::AdminError::connection("成员查询连接失败"));
    run(&mock, &mut state).await.unwrap();
    state = load(dir.path()).unwrap();
    assert!(!state.paused);
    assert!(!state.allows(Some("key"), "a", false, 1000));
    run(&mock, &mut state).await.unwrap();
    assert!(state.allows(Some("key"), "a", false, 1000));
    assert_eq!(*mock.probes.lock().unwrap(), ["a"]);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}

#[test]
fn monitor_recovery_never_resumes_explicit_pauses_or_non_transport_failures() {
    let dir = tempfile::tempdir().unwrap();
    for error in [
        "查询成员认证失败（HTTP 401）",
        "查询成员被拒绝（HTTP 403）",
        "席位冲突",
        "额度耗尽保护",
    ] {
        let mut state = ready_state();
        state.attention(error);
        assert!(!worker::can_retry_monitor_read(dir.path(), &state));
    }
    let mut state = ready_state();
    state.attention("成员查询连接失败");
    std::fs::write(worker::pause_path(dir.path()), "paused").unwrap();
    assert!(!worker::can_retry_monitor_read(dir.path(), &state));
    assert!(state.routing_unverified(Some("key"), false, 1000));
    assert!(!state.routing_unverified(Some("other"), false, 1000));
    assert!(!state.routing_unverified(Some("key"), true, 1000));
}

#[tokio::test]
async fn confirmed_exhaustion_requires_admin_validation_before_any_swap() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    let mut state = ready_state();
    *mock.snapshot_error.lock().unwrap() =
        Some(http::AdminError::connection("席位数量查询连接失败"));
    run(&mock, &mut state).await.unwrap();
    assert!(!state.paused);
    assert!(!state.allows(Some("key"), "a", false, 1000));
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(mock.probes.lock().unwrap().is_empty());

    mock.now.store(1000 + MONITOR_RETRY_SECS, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    assert!(state.allows(Some("key"), "b", false, mock.now()));
    assert!(mock.snapshot_reads.load(Ordering::SeqCst) > 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(*mock.probes.lock().unwrap(), ["b"]);
}

#[tokio::test]
async fn administrator_read_status_preserves_retryability_without_leaking_body() {
    use axum::{extract::Path, routing::get};
    let app = axum::Router::new().route(
        "/{status}/users",
        get(|Path(status): Path<u16>| async move {
            (
                axum::http::StatusCode::from_u16(status).unwrap(),
                "private-admin-detail",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for status in [400, 401, 403, 408, 429, 500, 502, 503, 504] {
        let api = http::AdminApi::mock(client.clone(), format!("http://{address}/{status}/users"));
        let error = api.snapshot().await.unwrap_err();
        assert_eq!(
            error.retryable(),
            status == 408 || status == 429 || status >= 500
        );
        assert!(!error.to_string().contains("private-admin-detail"));
    }
    let mut api = http::AdminApi::mock(client.clone(), format!("http://{address}/401/users"));
    let error = api
        .snapshot_with_refresh(|_| async {
            Ok(http::AdminApi::mock(
                client.clone(),
                format!("http://{address}/503/users"),
            ))
        })
        .await
        .unwrap_err();
    assert!(error.retryable());
    assert!(error.to_string().contains("刷新管理员凭据后"));
    server.abort();
}

#[test]
fn explicit_allowance_with_a_full_weekly_window_is_available_not_exhausted() {
    let raw = serde_json::json!({"rate_limit": {
        "allowed": true, "limit_reached": false,
        "primary_window": {"used_percent": 100, "limit_window_seconds": 604800, "reset_at": 9000},
        "secondary_window": null
    }, "model_usage": {"gpt-6-astra": {"available": true}}});
    assert_eq!(quota_evidence(&raw, 1000), (QuotaEvidence::Available, None));
    let mut denied = raw.clone();
    denied["rate_limit"]["allowed"] = serde_json::json!(false);
    denied["rate_limit"]["limit_reached"] = serde_json::json!(true);
    assert_eq!(
        quota_evidence(&denied, 1000),
        (QuotaEvidence::Exhausted, Some(9000))
    );
    for (allowed, reached) in [(true, true), (false, false)] {
        let mut contradictory = raw.clone();
        contradictory["rate_limit"]["allowed"] = allowed.into();
        contradictory["rate_limit"]["limit_reached"] = reached.into();
        assert_eq!(
            quota_evidence(&contradictory, 1000).0,
            QuotaEvidence::Unknown
        );
    }
    let mut missing = raw.clone();
    missing["rate_limit"]
        .as_object_mut()
        .unwrap()
        .remove("primary_window");
    assert_eq!(quota_evidence(&missing, 1000).0, QuotaEvidence::Unknown);
    assert_eq!(quota_evidence(&raw, 9000).0, QuotaEvidence::Unknown);
}

#[tokio::test]
async fn paused_holder_preserves_unknown_quota_and_recovers_exhaustion_without_admin_queries() {
    for unknown in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(dir.path());
        mock.holder_quota_unknown.store(unknown, Ordering::SeqCst);
        let mut state = ready_state();
        state.paused = true;
        std::fs::write(worker::pause_path(dir.path()), "paused").unwrap();
        run(&mock, &mut state).await.unwrap();
        state = load(dir.path()).unwrap();
        assert!(state.paused);
        assert!(worker::can_recheck_paused_holder(&state));
        assert_eq!(state.routing_unverified(Some("key"), false, 1000), !unknown);
        assert_eq!(state.validated_at, Some(1000));
        assert_eq!(state.next_check_at, Some(1000 + MONITOR_RETRY_SECS));

        if !unknown {
            mock.holder_quota_unknown.store(true, Ordering::SeqCst);
            run(&mock, &mut state).await.unwrap();
            assert!(state.routing_unverified(Some("key"), false, 1000));
        }
        mock.holder_quota_unknown.store(false, Ordering::SeqCst);
        mock.source_available.store(true, Ordering::SeqCst);
        mock.now.store(1000 + MONITOR_RETRY_SECS, Ordering::SeqCst);
        run(&mock, &mut state).await.unwrap();
        assert!(state.paused);
        assert!(worker::pause_path(dir.path()).exists());
        assert!(state.allows(Some("key"), "a", false, mock.now()));
        assert_eq!(state.validated_at, Some(1000));
        assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
        assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 0);
        assert!(mock.probes.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn legacy_paused_quota_failure_recovers_without_enabling_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    state.attention("调换已暂停，当前成员未确认额度可用");
    save(dir.path(), &state).unwrap();
    std::fs::write(worker::pause_path(dir.path()), "paused").unwrap();
    assert!(worker::can_recheck_paused_holder(&state));
    run(&mock, &mut state).await.unwrap();
    assert!(state.paused);
    assert!(state.allows(Some("key"), "a", false, mock.now()));
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn admin_refresh_permission_never_retries_rejected_or_unresolved_paused_operations() {
    let mut state = ready_state();
    state.paused = true;
    assert!(worker::can_refresh_admin(&state));
    for message in ["查询成员认证失败（HTTP 401）", "席位冲突", "额度耗尽保护"]
    {
        state.attention(message);
        assert!(!worker::can_refresh_admin(&state));
        assert!(!worker::can_recheck_paused_holder(&state));
    }
    state = ready_state();
    state.paused = true;
    state.holder_user_id = None;
    assert!(!worker::can_refresh_admin(&state));

    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.target_available.store(false, Ordering::SeqCst);
    state = self::state();
    run(&mock, &mut state).await.unwrap();
    assert!(state.pending());
    state.paused = true;
    assert!(!worker::can_refresh_admin(&state));
    assert!(!worker::can_recheck_paused_holder(&state));
}

#[tokio::test]
async fn initial_validation_retries_its_final_read_but_still_stops_on_rejection() {
    for retryable in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(dir.path());
        mock.source_available.store(true, Ordering::SeqCst);
        mock.snapshot_error_on_read.store(2, Ordering::SeqCst);
        *mock.snapshot_error.lock().unwrap() = Some(if retryable {
            http::AdminError::connection("成员查询连接失败")
        } else {
            "查询成员被拒绝（HTTP 403）".into()
        });
        let mut state = state();
        save(dir.path(), &state).unwrap();
        assert_eq!(run(&mock, &mut state).await.is_ok(), retryable);
        state = load(dir.path()).unwrap();
        assert!(!state.allows(Some("key"), "a", false, mock.now()));
        assert_eq!(state.paused, !retryable);
        assert_eq!(*mock.probes.lock().unwrap(), ["a"]);
        assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
        if retryable {
            assert_eq!(state.next_check_at, Some(1000 + MONITOR_RETRY_SECS));
            mock.now.store(1000 + MONITOR_RETRY_SECS, Ordering::SeqCst);
            run(&mock, &mut state).await.unwrap();
            assert!(state.allows(Some("key"), "a", false, mock.now()));
            assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn administrator_body_disconnect_retries_but_complete_invalid_json_stops() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for counts in [false, true] {
        for truncated in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = vec![0; 4096];
                    let n = socket.read(&mut request).await.unwrap();
                    let target = !counts
                        || String::from_utf8_lossy(&request[..n]).contains("/seat_type_counts");
                    let body = if target {
                        "private-admin-body"
                    } else {
                        r#"{"items":[{"id":"ua","seat_type":"prolite"}],"total":1}"#
                    };
                    let length = if target && truncated {
                        body.len() + 100
                    } else {
                        body.len()
                    };
                    let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}");
                    socket.write_all(response.as_bytes()).await.unwrap();
                    socket.shutdown().await.unwrap();
                    if target {
                        break;
                    }
                }
            });
            let api = http::AdminApi::mock(
                reqwest::Client::builder()
                    .no_proxy()
                    .retry(reqwest::retry::never())
                    .build()
                    .unwrap(),
                format!("http://{address}/users"),
            );
            let error = api.snapshot().await.unwrap_err();
            assert_eq!(error.retryable(), truncated);
            assert!(error.to_string().contains(if truncated {
                "读取失败"
            } else {
                "格式不兼容"
            }));
            assert!(!error.to_string().contains("private-admin-body"));
            server.await.unwrap();
        }
    }
}

#[tokio::test]
async fn unknown_quota_preserves_verified_route_without_snapshot_probe_or_swap() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    mock.holder_quota_unknown.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    run(&mock, &mut state).await.unwrap();
    state = load(dir.path()).unwrap();
    assert!(!state.paused);
    assert!(!state.routing_unverified(Some("key"), false, 1000));
    assert_eq!(state.validated_at, Some(1000));
    assert_eq!(state.next_check_at, Some(1000 + MONITOR_RETRY_SECS));
    mock.holder_quota_unknown.store(false, Ordering::SeqCst);
    mock.now.store(1060, Ordering::SeqCst);
    run(&mock, &mut state).await.unwrap();
    assert!(state.allows(Some("key"), "a", false, 1060));
    assert_eq!(state.validated_at, Some(1000));
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 0);
    assert!(mock.probes.lock().unwrap().is_empty());
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unknown_quota_cannot_publish_an_unverified_initial_holder() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.holder_quota_unknown.store(true, Ordering::SeqCst);
    let mut state = state();
    run(&mock, &mut state).await.unwrap();
    assert!(state.routing_unverified(Some("key"), false, mock.now()));
    assert_eq!(state.validated_at, None);
    assert_eq!(mock.snapshot_reads.load(Ordering::SeqCst), 1);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert!(mock.probes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn legacy_unknown_quota_pause_revalidates_without_forcing_a_seat_swap() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::new(dir.path());
    mock.source_available.store(true, Ordering::SeqCst);
    let mut state = ready_state();
    state.attention("实时订阅窗口无法判断；未提交席位调换");
    save(dir.path(), &state).unwrap();
    assert!(worker::can_retry_monitor_read(dir.path(), &state));
    run(&mock, &mut state).await.unwrap();
    assert!(!state.paused);
    assert!(state.allows(Some("key"), "a", false, 1000));
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
}
