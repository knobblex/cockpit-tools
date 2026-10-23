use super::*;

struct StorageGuard {
    path: PathBuf,
    previous: Option<std::ffi::OsString>,
    previous_data_dir: Option<std::ffi::OsString>,
}

impl StorageGuard {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("codex-refresh-cas-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        let previous = std::env::var_os("COCKPIT_TOOLS_TEST_DATA_DIR");
        let previous_data_dir = std::env::var_os("COCKPIT_TOOLS_DATA_DIR");
        std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", &path);
        // The cross-process token lease uses resolve_data_dir, not get_data_dir.
        std::env::set_var("COCKPIT_TOOLS_DATA_DIR", &path);
        assert!(get_accounts_dir().starts_with(&path));
        assert!(codex_token_refresh_file_lock_path("fixture").starts_with(&path));
        Self {
            path,
            previous,
            previous_data_dir,
        }
    }
}

impl Drop for StorageGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_TEST_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_TEST_DATA_DIR"),
        }
        match &self.previous_data_dir {
            Some(value) => std::env::set_var("COCKPIT_TOOLS_DATA_DIR", value),
            None => std::env::remove_var("COCKPIT_TOOLS_DATA_DIR"),
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn tokens(label: &str) -> CodexTokens {
    CodexTokens {
        id_token: format!("id-{label}"),
        access_token: format!("access-{label}"),
        refresh_token: Some(format!("refresh-{label}")),
    }
}

fn seed() -> CodexAccount {
    let account = CodexAccount::new(
        format!("refresh-cas-{}", uuid::Uuid::new_v4()),
        "test@example.com".into(),
        tokens("old"),
    );
    save_account(&account).unwrap();
    account
}

#[test]
fn refresh_response_preserves_concurrent_metadata_and_quota_write() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let original = seed();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let response_barrier = barrier.clone();
    let response_account = original.clone();
    let response = std::thread::spawn(move || {
        // The refresh request already captured its credentials; release the
        // simulated response only after a separate writer updates storage.
        response_barrier.wait();
        persist_refreshed_account(&response_account, tokens("rotated"))
    });
    let mut latest = original.clone();
    latest.account_note = Some("edited during refresh".into());
    latest.tags = Some(vec!["keep".into()]);
    latest.last_client_auth_observed_at = Some(123);
    latest.quota = Some(
        serde_json::from_value(serde_json::json!({
            "hourly_percentage": 70,
            "hourly_reset_time": 12345,
            "weekly_percentage": 80,
            "weekly_reset_time": 56789
        }))
        .unwrap(),
    );
    save_account(&latest).unwrap();
    barrier.wait();
    let (returned, saved) = response.join().unwrap().unwrap();
    assert!(saved);
    let persisted = load_account(&original.id).unwrap();
    for account in [&returned, &persisted] {
        assert_eq!(account.account_note, latest.account_note);
        assert_eq!(account.tags, latest.tags);
        assert_eq!(account.last_client_auth_observed_at, Some(123));
        assert_eq!(account.quota.as_ref().unwrap().hourly_percentage, 70);
        assert_eq!(account.quota.as_ref().unwrap().weekly_percentage, 80);
        assert_eq!(
            account.tokens.refresh_token,
            tokens("rotated").refresh_token
        );
        assert_eq!(account.token_generation, original.token_generation + 1);
    }
}

#[test]
fn refresh_response_cannot_overwrite_reauthorized_credentials_at_same_generation() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let original = seed();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let response_barrier = barrier.clone();
    let response_account = original.clone();
    let response = std::thread::spawn(move || {
        response_barrier.wait();
        persist_refreshed_account(&response_account, tokens("stale-response"))
    });
    let mut reauthorized = original.clone();
    reauthorized.tokens = tokens("reauthorized");
    save_account(&reauthorized).unwrap();
    barrier.wait();
    let (returned, saved) = response.join().unwrap().unwrap();
    assert!(!saved);
    assert_eq!(
        returned.tokens.access_token,
        reauthorized.tokens.access_token
    );
    let persisted = load_account(&original.id).unwrap();
    assert_eq!(
        persisted.tokens.refresh_token,
        reauthorized.tokens.refresh_token
    );
    assert_eq!(persisted.token_generation, original.token_generation);
}

#[test]
fn failed_old_refresh_cannot_mark_new_credentials_as_reauth_required() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let original = seed();
    let mut reauthorized = original.clone();
    reauthorized.tokens = tokens("reauthorized");
    reauthorized.token_generation += 1;
    save_account(&reauthorized).unwrap();
    let (returned, marked) = update_account_after_refresh_if_current(&original, |current| {
        current.requires_reauth = true;
        current.reauth_reason = Some("invalid_grant".into());
    })
    .unwrap();
    assert!(!marked);
    assert!(!returned.requires_reauth);
    assert!(!load_account(&original.id).unwrap().requires_reauth);
}

#[test]
fn failed_current_refresh_preserves_concurrent_user_edits() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let original = seed();
    let mut edited = original.clone();
    edited.account_note = Some("keep after failure".into());
    save_account(&edited).unwrap();
    let (returned, marked) = update_account_after_refresh_if_current(&original, |current| {
        current.requires_reauth = true;
        current.reauth_reason = Some("invalid_grant".into());
    })
    .unwrap();
    assert!(marked);
    assert!(returned.requires_reauth);
    let persisted = load_account(&original.id).unwrap();
    assert!(persisted.requires_reauth);
    assert_eq!(persisted.account_note, edited.account_note);
    assert_eq!(persisted.tokens.access_token, original.tokens.access_token);
}

#[test]
fn refresh_response_cannot_recreate_deleted_account() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let original = seed();
    delete_account_file(&original.id).unwrap();
    assert!(persist_refreshed_account(&original, tokens("rotated")).is_err());
    assert!(load_account(&original.id).is_none());
    assert!(!get_accounts_dir()
        .join(format!("{}.json", original.id))
        .exists());
}

#[test]
fn force_refresh_transport_failures_do_not_claim_expiration_or_require_reauth() {
    for raw in [
        "Token 刷新请求失败: error sending request for url (https://auth.openai.com/oauth/token)",
        "Token 刷新请求失败: operation timed out",
        "Token 刷新失败: status=503 Service Unavailable, body_len=42",
        "解析 Token 响应失败: invalid JSON",
    ] {
        assert!(matches!(
            super::classify_refresh_error(raw),
            super::CodexRefreshErrorKind::Other
        ));
        assert!(!super::is_reauth_required_refresh_error(raw));
        let formatted = format_refresh_error_for_user(raw);
        assert!(formatted.contains(raw));
        for misleading in ["已过期", "expired", "请重新登录", "sign in again"] {
            assert!(!formatted.contains(misleading), "{formatted}");
        }
    }

    for code in [
        "PROXY_RUNTIME_BUSY",
        "PROXY_ENGINE_MISSING",
        "PROXY_RUNTIME_READ_TIMEOUT",
    ] {
        assert_eq!(format_refresh_error_for_user(code), code);
        assert!(!super::is_reauth_required_refresh_error(code));
    }
}

#[test]
fn quota_recovery_exhaustion_preserves_current_metadata_and_cached_quota() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let original = seed();
    let mut edited = original.clone();
    edited.account_note = Some("edited while quota retry was in flight".into());
    edited.quota = Some(
        serde_json::from_value(serde_json::json!({
            "hourly_percentage": 70,
            "weekly_percentage": 80
        }))
        .unwrap(),
    );
    edited.usage_updated_at = Some(123);
    save_account(&edited).unwrap();
    let reason = "CODEX_QUOTA_AUTH_RECOVERY_EXHAUSTED: retry returned 401";
    let runtime = tokio::runtime::Runtime::new().unwrap();

    assert!(runtime
        .block_on(mark_quota_auth_recovery_exhausted(
            &original,
            reason,
            Instant::now(),
        ))
        .unwrap());

    let stored = load_account(&original.id).unwrap();
    assert!(stored.requires_reauth);
    assert_eq!(stored.reauth_reason.as_deref(), Some(reason));
    assert_eq!(stored.account_note, edited.account_note);
    assert_eq!(stored.quota.unwrap().hourly_percentage, 70);
    assert_eq!(stored.usage_updated_at, Some(123));
    assert_eq!(stored.tokens.access_token, original.tokens.access_token);
}

#[test]
fn stale_quota_recovery_cannot_mark_reauthorized_or_switched_account() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let original = seed();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for replacement in 0..4 {
        let mut current = original.clone();
        match replacement {
            0 => current.tokens = tokens("same-generation-reauthorized"),
            1 => current.token_generation += 1,
            2 => current.account_id = Some("another-workspace".into()),
            _ => current.user_id = Some("another-user".into()),
        }
        save_account(&current).unwrap();
        assert!(!runtime
            .block_on(mark_quota_auth_recovery_exhausted(
                &original,
                "CODEX_QUOTA_AUTH_RECOVERY_EXHAUSTED: stale retry",
                Instant::now(),
            ))
            .unwrap());
        assert!(!load_account(&original.id).unwrap().requires_reauth);
    }
}

#[test]
fn quota_recovery_reuses_new_credentials_without_rotating_or_scanning_profiles() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let original = seed();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for generation in [original.token_generation, original.token_generation + 1] {
        let mut current = original.clone();
        current.tokens.access_token = "at-newer-opaque-token".into();
        current.tokens.refresh_token = Some("newer-refresh-token".into());
        current.token_generation = generation;
        save_account(&current).unwrap();

        for force in [false, true] {
            let (reused, committed) = runtime
                .block_on(recover_account_for_quota_query(&original, force))
                .expect("a newer credential snapshot must be reused before profile/network work");

            assert!(!committed);
            assert_eq!(reused.token_generation, generation);
            assert_eq!(reused.tokens.access_token, current.tokens.access_token);
            assert_eq!(reused.tokens.refresh_token, current.tokens.refresh_token);
            assert!(!load_account(&original.id).unwrap().requires_reauth);
        }
    }
}

#[test]
fn quota_recovery_rejects_changed_workspace_before_profile_or_network_work() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let original = seed();
    let mut current = original.clone();
    current.account_id = Some("another-workspace".into());
    save_account(&current).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for force in [false, true] {
        assert_eq!(
            runtime
                .block_on(recover_account_for_quota_query(&original, force))
                .unwrap_err(),
            "CODEX_STALE_QUOTA_REQUEST"
        );
    }
    assert!(!load_account(&original.id).unwrap().requires_reauth);
}

#[test]
fn missing_refresh_token_failure_is_snapshot_guarded() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let mut original = seed();
    original.tokens.refresh_token = None;
    save_account(&original).unwrap();
    let mut current = original.clone();
    current.account_note = Some("preserve concurrent edit".into());
    save_account(&current).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(perform_managed_token_refresh_with_outcome(
            original.clone(),
            "quota missing RT test",
            true,
        ))
        .unwrap_err();
    assert_eq!(error, CODEX_MISSING_REFRESH_TOKEN_REAUTH_REASON);
    let marked = load_account(&original.id).unwrap();
    assert!(marked.requires_reauth);
    assert_eq!(marked.account_note, current.account_note);

    current.tokens = tokens("reauthorized");
    current.requires_reauth = false;
    current.reauth_reason = None;
    save_account(&current).unwrap();
    let (reused, committed) = runtime
        .block_on(perform_managed_token_refresh_with_outcome(
            original,
            "stale quota missing RT test",
            true,
        ))
        .unwrap();
    assert!(!committed);
    assert_eq!(reused.tokens.access_token, current.tokens.access_token);
    assert!(!load_account(&current.id).unwrap().requires_reauth);
}

#[test]
fn quota_recovery_failure_cannot_override_newer_success_with_same_credentials() {
    let _lock = crate::modules::test_support::env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _storage = StorageGuard::new();
    let original = seed();
    let started = Instant::now();
    let quota = serde_json::from_value(serde_json::json!({
        "hourly_percentage": 65,
        "weekly_percentage": 75
    }))
    .unwrap();
    save_refreshed_quota(
        &original,
        quota,
        None,
        started + Duration::from_millis(1),
    )
    .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    assert!(!runtime
        .block_on(mark_quota_auth_recovery_exhausted(
            &original,
            "CODEX_QUOTA_AUTH_RECOVERY_EXHAUSTED: older failure",
            started,
        ))
        .unwrap());

    let stored = load_account(&original.id).unwrap();
    assert!(!stored.requires_reauth);
    assert_eq!(stored.quota.unwrap().hourly_percentage, 65);
}

#[test]
fn quota_recovery_allows_only_committed_refresh_to_enrich_unknown_claims() {
    let expected = CodexAccount::new(
        "quota-identity-enrichment".into(),
        "test@example.com".into(),
        tokens("original"),
    );
    let mut refreshed = expected.clone();
    refreshed.user_id = Some("user-current".into());
    refreshed.account_id = Some("workspace-current".into());
    refreshed.organization_id = Some("org-current".into());
    assert!(quota_recovery_result_identity_matches(&refreshed, &expected, true));
    assert!(!quota_recovery_result_identity_matches(&refreshed, &expected, false));

    for changed in 0..5 {
        let mut incompatible = refreshed.clone();
        match changed {
            0 => incompatible.user_id = Some("another-user".into()),
            1 => incompatible.account_id = Some("another-workspace".into()),
            2 => incompatible.organization_id = Some("another-org".into()),
            3 => incompatible.email = "another@example.com".into(),
            _ => incompatible.token_source_mode = CODEX_TOKEN_SOURCE_WEB_SESSION.into(),
        }
        assert!(!quota_recovery_result_identity_matches(&incompatible, &refreshed, true));
    }
}
