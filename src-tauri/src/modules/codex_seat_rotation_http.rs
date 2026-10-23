use super::{Member, Operation};
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Client,
};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::future::Future;

pub type Snapshot = (Vec<Member>, BTreeMap<String, u64>);

#[derive(Debug)]
pub struct AdminError {
    unauthorized: bool,
    retryable: bool,
    message: String,
}
impl From<&str> for AdminError {
    fn from(message: &str) -> Self {
        Self {
            unauthorized: false,
            retryable: false,
            message: message.into(),
        }
    }
}
impl From<String> for AdminError {
    fn from(message: String) -> Self {
        Self {
            unauthorized: false,
            retryable: false,
            message,
        }
    }
}
impl From<AdminError> for String {
    fn from(error: AdminError) -> Self {
        error.message
    }
}
impl AdminError {
    pub fn retryable(&self) -> bool {
        self.retryable
    }

    pub(crate) fn connection(message: &str) -> Self {
        Self {
            unauthorized: false,
            retryable: true,
            message: message.into(),
        }
    }

    // Only these old, transport-only failures can be migrated out of attention.
    pub(crate) fn legacy_connection_failure(message: &str) -> bool {
        matches!(message, "成员查询连接失败" | "席位数量查询连接失败")
    }
}
impl std::fmt::Display for AdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for AdminError {}

pub struct AdminApi {
    client: Client,
    base: String,
    headers: HeaderMap,
    token_generation: u64,
}

impl AdminApi {
    pub fn new(
        client: Client,
        token: &str,
        workspace: &str,
        token_generation: u64,
    ) -> Result<Self, String> {
        uuid::Uuid::parse_str(workspace).map_err(|_| "无效的工作区 ID")?;
        let mut headers = HeaderMap::new();
        let mut bearer =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| "管理员凭据无效")?;
        bearer.set_sensitive(true);
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(
            "chatgpt-account-id",
            HeaderValue::from_str(workspace).map_err(|_| "工作区无效")?,
        );
        Ok(Self {
            client,
            base: format!("https://chatgpt.com/backend-api/accounts/{workspace}/users"),
            headers,
            token_generation,
        })
    }

    #[cfg(test)]
    pub(super) fn mock(client: Client, base: String) -> Self {
        Self {
            client,
            base,
            headers: HeaderMap::new(),
            token_generation: 0,
        }
    }

    pub async fn snapshot(&self) -> Result<Snapshot, AdminError> {
        #[derive(Deserialize)]
        struct Page {
            items: Vec<Member>,
            total: usize,
        }
        #[derive(Deserialize)]
        struct Counts {
            seat_type_counts: BTreeMap<String, u64>,
        }
        let mut members = Vec::new();
        let mut ids = HashSet::new();
        let mut total = None;
        loop {
            let response = self
                .client
                .get(&self.base)
                .headers(self.headers.clone())
                .query(&[("limit", 25), ("offset", members.len())])
                .send()
                .await
                .map_err(|_| AdminError::connection("成员查询连接失败"))?;
            check_status(response.status(), "查询成员")?;
            let page: Page = response.json().await.map_err(|_| "成员响应格式不兼容")?;
            if page.total > 5000
                || total.is_some_and(|n| n != page.total)
                || (page.items.is_empty() && members.len() < page.total)
            {
                return Err("成员列表在查询时发生变化，请重试".into());
            }
            total = Some(page.total);
            for member in page.items {
                if member.id.is_empty() || !ids.insert(member.id.clone()) {
                    return Err("成员响应包含重复或缺失身份".into());
                }
                members.push(member);
            }
            if members.len() == page.total {
                break;
            }
            if members.len() > page.total {
                return Err("成员数量不一致".into());
            }
        }
        let response = self
            .client
            .get(format!("{}/seat_type_counts", self.base))
            .headers(self.headers.clone())
            .send()
            .await
            .map_err(|_| AdminError::connection("席位数量查询连接失败"))?;
        check_status(response.status(), "查询席位数量")?;
        let counts: Counts = response
            .json()
            .await
            .map_err(|_| "席位数量响应格式不兼容")?;
        if counts.seat_type_counts.get("prolite") != Some(&1) {
            return Err("初版要求工作区恰好有一个高级席位".into());
        }
        let mut actual = BTreeMap::new();
        for member in &members {
            *actual.entry(member.seat_type.clone()).or_insert(0u64) += 1;
        }
        let nonzero = counts
            .seat_type_counts
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .collect::<BTreeMap<_, _>>();
        if actual != nonzero {
            return Err("成员席位与席位统计不一致，请重新核对".into());
        }
        Ok((members, nonzero))
    }

    /// Only read-only reconciliation may renew credentials and retry once.
    /// Retain the refreshed client for subsequent snapshots and the single POST.
    pub async fn snapshot_with_refresh<F, Fut>(
        &mut self,
        refresh: F,
    ) -> Result<Snapshot, AdminError>
    where
        F: FnOnce(u64) -> Fut,
        Fut: Future<Output = Result<Self, String>>,
    {
        match self.snapshot().await {
            Ok(snapshot) => Ok(snapshot),
            Err(error) if error.unauthorized => {
                *self = refresh(self.token_generation)
                    .await
                    .map_err(AdminError::from)?;
                self.snapshot().await.map_err(|mut error| {
                    error.message = format!("刷新管理员凭据后仍未通过只读核对：{error}");
                    error
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Exactly one POST; caller persists the attempt and reconciles even on errors.
    /// No seat/update, purchase endpoint, redirect, or application retry path.
    pub async fn swap(&self, operation: &Operation) -> Result<(), String> {
        let response = self
            .client
            .post(format!("{}/swap-seats", self.base))
            .headers(self.headers.clone())
            .json(&super::swap_body(operation))
            .send()
            .await
            .map_err(|_| "调换结果未知，需要核对远端")?;
        check_status(response.status(), "提交席位调换").map_err(|error| error.to_string())
    }
}

fn check_status(status: reqwest::StatusCode, stage: &str) -> Result<(), AdminError> {
    if status.is_success() {
        return Ok(());
    }
    let message = match status.as_u16() {
        401 => format!("{stage}认证失败（HTTP 401）；当前管理员凭据被接口拒绝，需重新核对授权，不能据此认定成员管理权限不足"),
        403 => format!("{stage}被拒绝（HTTP 403）；请核对管理员角色、工作区及当前授权的成员管理权限"),
        429 => format!("{stage}受到限流（HTTP 429），需要稍后重新核对"),
        code => format!("{stage}接口失败（HTTP {code}），需要重新核对"),
    };
    Err(AdminError {
        unauthorized: status.as_u16() == 401,
        retryable: status.is_server_error() || matches!(status.as_u16(), 408 | 429),
        message,
    })
}
