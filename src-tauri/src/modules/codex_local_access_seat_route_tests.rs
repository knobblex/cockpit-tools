use super::*;

#[test]
fn incomplete_quota_metadata_is_unknown_but_explicit_free_is_rejected() {
    let available = json!({"rate_limit": {
        "allowed": true, "limit_reached": false,
        "primary_window": {"used_percent": 25, "limit_window_seconds": 18000, "reset_at": 2000}
    }});
    for plan in [None, Some(""), Some(" ")] {
        assert_eq!(
            seat_live_quota_evidence(plan, Some(&available), 1000).unwrap(),
            (seat::QuotaEvidence::Unknown, None)
        );
    }
    assert_eq!(
        seat_live_quota_evidence(Some("prolite"), None, 1000).unwrap(),
        (seat::QuotaEvidence::Unknown, None)
    );
    assert_eq!(
        seat_live_quota_evidence(Some("prolite"), Some(&available), 1000).unwrap(),
        (seat::QuotaEvidence::Available, None)
    );
    assert!(seat_live_quota_evidence(Some("free"), Some(&available), 1000).is_err());
}

#[test]
fn quota_preparation_retries_only_known_transport_failures_for_the_same_eligible_holder() {
    let mut holder = CodexAccount::new(
        "fixture-holder".into(),
        "holder@example.invalid".into(),
        crate::models::codex::CodexTokens {
            access_token: "fixture-access".into(),
            refresh_token: Some("fixture-refresh".into()),
            id_token: String::new(),
        },
    );
    holder.account_id = Some("00000000-0000-4000-8000-000000000001".into());
    holder.user_id = Some("fixture-holder-user".into());
    holder.account_structure = Some("workspace".into());
    for language in ["zh-CN", "en", "ja"] {
        for raw in [
            "Token 刷新请求失败: private-detail",
            "读取响应失败: private-detail",
            "Token 刷新失败: status=429 Too Many Requests, body_len=5",
            "Token 刷新失败: status=503 Service Unavailable, body_len=5",
        ] {
            let error = crate::modules::i18n::translate(
                language,
                "codex.authError.refreshFailed",
                &[("error", raw)],
            );
            let classified = seat_quota_preparation_error(&holder, Some(&holder), &error, language);
            assert!(classified.retryable());
            assert!(!String::from(classified).contains("private-detail"));
        }
    }
    for error in [
        "Token 刷新失败: status=401 Unauthorized",
        "Token 刷新失败: status=400 Bad Request",
        "keychain decrypt failed",
        "账号不存在",
        "unrelated Token 刷新请求失败: private-detail",
    ] {
        assert!(!seat_quota_preparation_error(&holder, Some(&holder), error, "en").retryable());
    }
    let network = "Token 刷新请求失败: private-detail";
    assert!(!seat_quota_preparation_error(&holder, None, network, "en").retryable());
    for change in 0..3 {
        let mut invalid = holder.clone();
        match change {
            0 => invalid.requires_reauth = true,
            1 => invalid.user_id = Some("different-member".into()),
            _ => invalid.account_structure = Some("personal".into()),
        }
        assert!(!seat_quota_preparation_error(&holder, Some(&invalid), network, "en").retryable());
    }
}

// Run in a child process: stack overflow aborts rather than unwinds, and the
// account/data-path globals must never point a fixture at the user's profile.
#[test]
fn seat_validation_survives_request_stack_pressure() {
    const CHILD: &str = "COCKPIT_SEAT_STACK_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "seat_validation_survives_request_stack_pressure",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("COCKPIT_TOOLS_DATA_DIR", dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated seat validation failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let dir = seat_dir().unwrap();
    let workspace = "00000000-0000-4000-8000-000000000001";
    let mut holder = CodexAccount::new(
        "fixture-holder".into(),
        "holder@example.invalid".into(),
        crate::models::codex::CodexTokens {
            access_token: "fixture-access".into(),
            refresh_token: Some("fixture-refresh".into()),
            id_token: String::new(),
        },
    );
    holder.account_id = Some(workspace.into());
    holder.user_id = Some("fixture-holder-user".into());
    holder.plan_type = Some("team".into());
    holder.account_structure = Some("workspace".into());
    let mut admin = holder.clone();
    admin.id = "fixture-admin".into();
    admin.user_id = Some("fixture-admin-user".into());
    let admin_path = dir.join("codex_accounts/fixture-admin.json");
    fs::create_dir_all(admin_path.parent().unwrap()).unwrap();
    let write_admin = |admin: &CodexAccount| {
        let encrypted =
            crate::modules::secure_account_storage::serialize_account_file("codex", admin).unwrap();
        fs::write(&admin_path, encrypted).unwrap();
    };
    write_admin(&admin);
    let mut state = seat::State {
        config: Some(seat::Config {
            enabled: true,
            workspace_id: workspace.into(),
            admin_account_id: admin.id.clone(),
            api_key_id: "fixture-key".into(),
            use_api_key_pool: true,
            member_account_ids: Vec::new(),
            probe_model: String::new(),
        }),
        phase: seat::Phase::Ready,
        holder_account_id: Some(holder.id.clone()),
        holder_user_id: holder.user_id.clone(),
        validated_at: Some(seat_now()),
        ..seat::State::default()
    };
    seat::save(&dir, &state).unwrap();
    let holder_path = dir.join("codex_accounts/fixture-holder.json");
    fs::write(
        &holder_path,
        crate::modules::secure_account_storage::serialize_account_file("codex", &holder).unwrap(),
    )
    .unwrap();
    let mut legacy = state.config.clone().unwrap();
    legacy.use_api_key_pool = false;
    legacy.member_account_ids = vec![holder.id.clone(), "missing-candidate".into()];
    legacy.probe_model = api_service_routable_codex_model_ids()
        .into_iter()
        .find(|model| native_default_codex_model(model) && !native_is_reserve_model(model))
        .expect("at least one ordinary Codex model is routable");
    let mut collection: CodexLocalAccessCollection = serde_json::from_value(json!({
        "enabled": true, "port": 1455, "apiKey": "fixture-secret",
        "accountIds": [holder.id, "missing-candidate"], "createdAt": 1, "updatedAt": 2,
        "apiKeys": [{"id": "fixture-key", "label": "Fixture", "key": "fixture-secret"}]
    }))
    .unwrap();
    assert!(seat_candidate_ids(&legacy, &collection).is_err());
    seat_check_holder_scope(&legacy, &collection, &holder.id).unwrap();
    collection.account_ids.retain(|id| id != &holder.id);
    assert!(seat_check_holder_scope(&legacy, &collection, &holder.id).is_err());
    collection.account_ids.push(holder.id.clone());
    legacy.member_account_ids = vec!["missing-candidate".into()];
    assert!(seat_check_holder_scope(&legacy, &collection, &holder.id).is_err());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let runtime = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            validate_with_stack_pressure(&runtime, &holder);
            (runtime, holder)
        })
        .unwrap()
        .join()
        .unwrap();
    let (runtime, holder) = runtime;

    // Normal routing depends on the verified holder, not on the administrator
    // still being available or on how long ago the seat was verified.
    admin.account_id = Some("00000000-0000-4000-8000-000000000002".into());
    write_admin(&admin);
    runtime
        .block_on(async { seat_validate_prepared_route(&holder, Some("fixture-key"), false) })
        .unwrap();
    fs::remove_file(&admin_path).unwrap();
    state.validated_at = Some(seat_now() - 30 * 24 * 60 * 60);
    seat::save(&dir, &state).unwrap();
    runtime
        .block_on(async { seat_validate_prepared_route(&holder, Some("fixture-key"), false) })
        .unwrap();

    // The actual holder's identity and authorization remain mandatory at the
    // final route-preparation boundary.
    for change in 0..3 {
        let mut invalid = holder.clone();
        match change {
            0 => invalid.account_id = Some("00000000-0000-4000-8000-000000000002".into()),
            1 => invalid.user_id = Some("different-member".into()),
            _ => invalid.requires_reauth = true,
        }
        let error = runtime
            .block_on(async { seat_validate_prepared_route(&invalid, Some("fixture-key"), false) })
            .unwrap_err();
        assert_eq!(error.code, "premium_seat_not_verified");
    }
    state.phase = seat::Phase::Idle;
    state.validated_at = None;
    seat::save(&dir, &state).unwrap();
    let error = runtime
        .block_on(async { seat_validate_prepared_route(&holder, Some("fixture-key"), false) })
        .unwrap_err();
    assert_eq!(error.code, "premium_seat_not_verified");
}

#[inline(never)]
fn validate_with_stack_pressure(runtime: &tokio::runtime::Runtime, holder: &CodexAccount) {
    // Model the deeply nested request/continuation polling stack from the macOS
    // crash. Keep this frame live while the final routing state is read.
    let mut request_stack = [0u8; 1792 * 1024];
    std::hint::black_box(&mut request_stack);
    runtime
        .block_on(async { seat_validate_prepared_route(holder, Some("fixture-key"), false) })
        .unwrap();
    std::hint::black_box(&request_stack);
}
