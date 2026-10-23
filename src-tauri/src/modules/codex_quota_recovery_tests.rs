use super::*;
use crate::models::codex::{CodexAuthMode, CodexTokens};
use std::{cell::RefCell, collections::VecDeque, future::ready};

const EXPIRED: &str = "API 返回错误 401 Unauthorized [error_code:token_expired] [body_len:334] [request-id:-] [x-request-id:-] [cf-ray:a4e6fb155dd6ce6d-SIN] [body:{\"error\":{\"message\":\"Your authentication token has expired. Please try refreshing it.\",\"code\":\"token_expired\"}}]";

fn account(label: &str, generation: u64) -> CodexAccount {
    let mut account = CodexAccount::new(
        "quota-recovery".into(),
        "test@example.com".into(),
        CodexTokens {
            id_token: String::new(),
            access_token: format!("at-{label}"),
            refresh_token: Some(format!("rt-{label}")),
        },
    );
    account.token_generation = generation;
    account.account_id = Some("workspace".into());
    account
}

fn success() -> Result<FetchQuotaResult, String> {
    Ok(FetchQuotaResult {
        quota: serde_json::from_value(json!({
            "hourly_percentage": 80, "weekly_percentage": 60,
        })).unwrap(),
        plan_type: Some("plus".into()),
    })
}

struct Outcome {
    result: Result<FetchQuotaResult, String>,
    account: CodexAccount,
    events: Vec<String>,
    marked: Vec<(u64, String)>,
}

async fn run(
    mut account: CodexAccount,
    responses: Vec<Result<FetchQuotaResult, String>>,
    recoveries: Vec<Result<(CodexAccount, bool), String>>,
    mark_current: bool,
) -> Outcome {
    let mut responses = VecDeque::from(responses);
    let mut recoveries = VecDeque::from(recoveries);
    let events = RefCell::new(Vec::new());
    let mut marked = Vec::new();
    let result = fetch_quota_with_auth_recovery_using(
        &mut account,
        |snapshot| {
            events.borrow_mut().push(format!("get:{}", snapshot.token_generation));
            ready(responses.pop_front().expect("unexpected extra quota request"))
        },
        |snapshot, force| {
            events.borrow_mut().push(format!("recover:{}:{force}", snapshot.token_generation));
            ready(recoveries.pop_front().expect("unexpected extra recovery"))
        },
        |snapshot, reason| {
            marked.push((snapshot.token_generation, reason));
            ready(Ok(mark_current))
        },
    ).await;
    assert!(responses.is_empty(), "expected quota request was skipped");
    assert!(recoveries.is_empty(), "expected recovery was skipped");
    Outcome { result, account, events: events.into_inner(), marked }
}

#[test]
fn quota_recovery_requires_an_actual_recoverable_http_401() {
    for error in [
        EXPIRED,
        "API 返回错误 401 Unauthorized",
        "API 返回错误 401 [error_code:invalid_token]",
        "API 返回错误 401 Unauthorized [body:[error_code:token_revoked]]",
        "API 返回错误 401 Unauthorized [request-id:error_code=token_revoked]",
    ] {
        assert!(is_recoverable_quota_auth_error(error), "{error}");
    }
    for error in [
        "API 返回错误 403 Forbidden [error_code:token_expired]",
        "API 返回错误 429 [body:401 Unauthorized token_expired]",
        "API 返回错误 500 [request-id:401]",
        "API 返回错误 4010 [error_code:token_expired]",
        "API 返回错误 401 [error_code:token_revoked]",
        "API 返回错误 401 [error_code:token_invalidated]",
        "Token 刷新失败: status=401 Unauthorized, error_code=invalid_grant",
        "PROXY_RUNTIME_BUSY",
    ] {
        assert!(!is_recoverable_quota_auth_error(error), "{error}");
    }
}

#[tokio::test]
async fn quota_recovery_uses_synchronized_credentials_without_rotating() {
    let outcome = run(account("old", 1), vec![Err(EXPIRED.into()), success()],
        vec![Ok((account("synced", 2), false))], true).await;
    assert!(outcome.result.is_ok());
    assert_eq!(outcome.events, ["get:1", "recover:1:false", "get:2"]);
    assert_eq!(outcome.account.tokens.access_token, "at-synced");
    assert!(outcome.marked.is_empty());
}

#[tokio::test]
async fn quota_recovery_retries_same_generation_replacement_before_rotation() {
    let outcome = run(account("old", 1), vec![Err(EXPIRED.into()), success()],
        vec![Ok((account("replaced", 1), false))], true).await;
    assert!(outcome.result.is_ok());
    assert_eq!(outcome.events, ["get:1", "recover:1:false", "get:1"]);
    assert_eq!(outcome.account.tokens.access_token, "at-replaced");
}

#[tokio::test]
async fn quota_recovery_refreshes_without_retrying_unchanged_credentials() {
    let outcome = run(account("old", 1), vec![Err(EXPIRED.into()), success()],
        vec![Ok((account("old", 1), false)), Ok((account("refreshed", 2), true))], true).await;
    assert!(outcome.result.is_ok());
    assert_eq!(outcome.events, ["get:1", "recover:1:false", "recover:1:true", "get:2"]);
    assert_eq!(outcome.account.token_generation, 2);
    assert!(outcome.marked.is_empty());
}

#[tokio::test]
async fn quota_recovery_refreshes_if_synchronized_token_is_also_rejected() {
    let outcome = run(account("old", 1), vec![Err(EXPIRED.into()), Err(EXPIRED.into()), success()],
        vec![Ok((account("synced", 2), false)), Ok((account("refreshed", 3), true))], true).await;
    assert!(outcome.result.is_ok());
    assert_eq!(outcome.events, ["get:1", "recover:1:false", "get:2", "recover:2:true", "get:3"]);
    assert_eq!(outcome.account.tokens.access_token, "at-refreshed");
    assert!(outcome.marked.is_empty());
}

#[tokio::test]
async fn quota_recovery_marks_only_after_committed_refresh_is_rejected() {
    let outcome = run(account("old", 1), vec![Err(EXPIRED.into()), Err(EXPIRED.into()), Err(EXPIRED.into())],
        vec![Ok((account("synced", 2), false)), Ok((account("refreshed", 3), true))], true).await;
    assert!(outcome.result.err().expect("rejected refreshed token").starts_with("CODEX_QUOTA_AUTH_RECOVERY_EXHAUSTED:"));
    assert_eq!(outcome.marked.len(), 1);
    assert_eq!(outcome.marked[0].0, 3);
    assert!(outcome.marked[0].1.contains(EXPIRED));
    assert_eq!(outcome.events.len(), 5);
}

#[tokio::test]
async fn quota_recovery_does_not_mark_a_reused_concurrent_refresh_terminal() {
    let outcome = run(account("old", 1), vec![Err(EXPIRED.into()), Err(EXPIRED.into())],
        vec![Ok((account("old", 1), false)), Ok((account("concurrent", 2), false))], true).await;
    assert_eq!(outcome.result.err().expect("rejected concurrent token"), EXPIRED);
    assert!(outcome.marked.is_empty());
}

#[tokio::test]
async fn quota_recovery_does_not_request_reauth_when_a_new_login_won() {
    let outcome = run(account("old", 1), vec![Err(EXPIRED.into()), Err(EXPIRED.into())],
        vec![Ok((account("old", 1), false)), Ok((account("refreshed", 2), true))], false).await;
    assert_eq!(outcome.result.err().expect("stale rejection"), EXPIRED);
    assert_eq!(outcome.marked.len(), 1);
}

#[tokio::test]
async fn quota_recovery_stops_on_transport_or_non_auth_errors_at_every_stage() {
    for error in ["请求失败: timed out", "PROXY_RUNTIME_BUSY", "API 返回错误 429 Too Many Requests", "API 返回错误 503 Service Unavailable"] {
        for stage in 0..3 {
            let responses = if stage == 0 { vec![Err(error.into())] } else { vec![Err(EXPIRED.into()), Err(error.into())] };
            let recoveries = match stage {
                0 => vec![],
                1 => vec![Ok((account("synced", 2), false))],
                _ => vec![Ok((account("old", 1), false)), Ok((account("refreshed", 2), true))],
            };
            let outcome = run(account("old", 1), responses, recoveries, true).await;
            assert_eq!(outcome.result.err().expect("request failure"), error);
            assert!(outcome.marked.is_empty());
        }
    }
}

#[tokio::test]
async fn quota_recovery_returns_actual_refresh_failure_not_the_first_401() {
    for error in [
        "Token 刷新请求失败: timed out",
        "Token 刷新失败: status=401 Unauthorized, error_code=refresh_token_expired",
        "Token 刷新失败: status=401 Unauthorized, error_code=refresh_token_reused",
        "Codex 登录授权缺少 refresh_token，无法自动续期",
        "CODEX_STALE_QUOTA_REQUEST",
    ] {
        let outcome = run(account("old", 1), vec![Err(EXPIRED.into())],
            vec![Ok((account("old", 1), false)), Err(error.into())], true).await;
        assert_eq!(outcome.result.err().expect("refresh failure"), error);
        assert!(outcome.marked.is_empty());
    }
}

#[tokio::test]
async fn quota_recovery_sync_failure_does_not_invalidate_login_or_rotate() {
    let error = "同步最新 Codex 凭据失败: credential store unavailable";
    let outcome = run(account("old", 1), vec![Err(EXPIRED.into())],
        vec![Err(error.into())], true).await;
    assert_eq!(outcome.result.err().expect("sync failure"), error);
    assert_eq!(outcome.events, ["get:1", "recover:1:false"]);
    assert!(outcome.marked.is_empty());
}

#[tokio::test]
async fn quota_recovery_does_not_rotate_for_other_auth_modes_or_success() {
    let outcome = run(account("old", 1), vec![success()], vec![], true).await;
    assert!(outcome.result.is_ok());
    let mut api_key = account("old", 1);
    api_key.auth_mode = CodexAuthMode::Apikey;
    let mut web_session = account("old", 1);
    web_session.token_source_mode = "chatgpt_web_session".into();
    for account in [api_key, web_session] {
        let outcome = run(account, vec![Err(EXPIRED.into())], vec![], true).await;
        assert_eq!(outcome.result.err().expect("unmanaged token rejection"), EXPIRED);
        assert_eq!(outcome.events, ["get:1"]);
        assert!(outcome.marked.is_empty());
    }
}
