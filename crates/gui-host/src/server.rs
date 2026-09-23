use std::net::{SocketAddr, TcpListener as StandardTcpListener};
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::header::{
    CACHE_CONTROL, COOKIE, HOST, PRAGMA, REFERRER_POLICY, SET_COOKIE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;

use crate::model::{
    ApplicationSnapshot, EvidencePageViewModel, ObserverRuntimeViewModel, SessionPageViewModel,
};
use crate::product::ProductService;
use crate::projection::{
    ProjectionCache, ProjectionConfiguration, load_observer_runtime, load_snapshot,
};

pub struct ServerConfiguration {
    pub bind_address: SocketAddr,
    pub ui_directory: PathBuf,
    pub access_token: String,
    pub product_service: ProductService,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

struct ApplicationState {
    projection_cache: Arc<ProjectionCache>,
    expected_host: String,
    access_token: String,
    product_service: Arc<ProductService>,
}

struct ApiError {
    status: StatusCode,
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticExportRequest {
    archive_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetentionApplicationRequest {
    confirmation_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionPageQuery {
    agent_id: String,
    cursor: String,
    limit: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvidencePageQuery {
    session_id: String,
    cursor: String,
    limit: usize,
}

#[derive(Deserialize)]
struct BootstrapQuery {
    view: Option<String>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

pub async fn run(configuration: ServerConfiguration) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(configuration.bind_address)
        .await
        .map_err(|error| {
            format!(
                "无法监听 GUI 地址 address={} error={error}",
                configuration.bind_address
            )
        })?;
    serve(configuration, listener).await
}

pub async fn run_with_listener(
    configuration: ServerConfiguration,
    listener: StandardTcpListener,
) -> Result<(), String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("GUI 监听器无法切换为非阻塞模式 error={error}"))?;
    let asynchronous_listener = tokio::net::TcpListener::from_std(listener)
        .map_err(|error| format!("GUI 监听器无法进入异步运行时 error={error}"))?;
    serve(configuration, asynchronous_listener).await
}

async fn serve(
    configuration: ServerConfiguration,
    listener: tokio::net::TcpListener,
) -> Result<(), String> {
    if !configuration.ui_directory.is_dir() {
        return Err(format!(
            "GUI 静态目录不存在 path={}",
            configuration.ui_directory.display()
        ));
    }

    let bind_address = configuration.bind_address;
    let state = Arc::new(ApplicationState {
        projection_cache: Arc::new(ProjectionCache::default()),
        expected_host: bind_address.to_string(),
        access_token: configuration.access_token,
        product_service: Arc::new(configuration.product_service),
    });
    let application = Router::new()
        .route("/api/v1/session/{token}", get(bootstrap_session))
        .route("/api/v1/snapshot", get(get_snapshot))
        .route("/api/v1/observer-runtime", get(get_observer_runtime))
        .route("/api/v1/sessions", get(get_sessions))
        .route("/api/v1/evidence", get(get_evidence))
        .route("/api/v1/product-settings", get(get_product_settings))
        .route("/api/v1/retention-plan", get(get_retention_plan))
        .route(
            "/api/v1/expired-observer-runs",
            delete(delete_expired_observer_runs),
        )
        .route("/api/v1/diagnostic-exports", post(post_diagnostic_export))
        .fallback_service(
            ServeDir::new(configuration.ui_directory).append_index_html_on_directories(true),
        )
        .layer(middleware::from_fn(add_security_headers))
        .with_state(state);
    println!("{{\"status\":\"running\",\"address\":\"{bind_address}\"}}");
    axum::serve(listener, application)
        .await
        .map_err(|error| format!("GUI 主机运行失败 address={bind_address} error={error}"))
}

async fn bootstrap_session(
    State(state): State<Arc<ApplicationState>>,
    Path(token): Path<String>,
    Query(query): Query<BootstrapQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    validate_host(&state, &headers)?;
    if token != state.access_token {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            message: String::from("GUI 会话令牌不匹配"),
        });
    }
    let target = match query.view.as_deref() {
        None => String::from("/"),
        Some(view)
            if [
                "overview",
                "sessions",
                "capabilities",
                "agents",
                "evidence",
                "settings",
            ]
            .contains(&view) =>
        {
            format!("/?view={view}")
        }
        Some(view) => {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                message: format!("未知 GUI 主页面 view={view}"),
            });
        }
    };
    let mut response = Redirect::temporary(&target).into_response();
    let cookie = format!(
        "agentreins_session={}; HttpOnly; SameSite=Strict; Path=/",
        state.access_token
    );
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("GUI 会话 Cookie 无法生成 error={error}"),
        })?,
    );
    Ok(response)
}

async fn get_snapshot(
    State(state): State<Arc<ApplicationState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;

    let result = load_current_snapshot(&state).await?;
    Ok(Json::<ApplicationSnapshot>(result).into_response())
}

async fn get_observer_runtime(
    State(state): State<Arc<ApplicationState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let runtime = tokio::task::spawn_blocking(load_observer_runtime)
        .await
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("Observer 生命周期查询任务异常结束 error={error}"),
        })?
        .map_err(internal_error)?;
    Ok(Json::<ObserverRuntimeViewModel>(runtime).into_response())
}

async fn get_sessions(
    State(state): State<Arc<ApplicationState>>,
    headers: HeaderMap,
    Query(query): Query<SessionPageQuery>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let snapshot = load_current_snapshot(&state).await?;
    if query.agent_id != snapshot.agent.id {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            message: format!(
                "会话分页 Agent 不匹配 expected={} actual={}",
                snapshot.agent.id, query.agent_id
            ),
        });
    }
    let total = snapshot.activities.len();
    let (start, end, next_cursor) = page_bounds(&query.cursor, query.limit, total)?;
    let items = snapshot
        .activities
        .into_iter()
        .skip(start)
        .take(end - start)
        .collect();
    let total = u64::try_from(total).map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("会话数量超出范围 error={error}"),
    })?;
    Ok(Json(SessionPageViewModel {
        items,
        total,
        next_cursor,
    })
    .into_response())
}

async fn get_evidence(
    State(state): State<Arc<ApplicationState>>,
    headers: HeaderMap,
    Query(query): Query<EvidencePageQuery>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let snapshot = load_current_snapshot(&state).await?;
    if query.session_id != snapshot.agent.semantic_session_id {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            message: format!(
                "证据分页会话不匹配 expected={} actual={}",
                snapshot.agent.semantic_session_id, query.session_id
            ),
        });
    }
    let total = snapshot.evidence.items.len();
    let (start, end, next_cursor) = page_bounds(&query.cursor, query.limit, total)?;
    let items = snapshot
        .evidence
        .items
        .into_iter()
        .skip(start)
        .take(end - start)
        .collect();
    let total = u64::try_from(total).map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("证据数量超出范围 error={error}"),
    })?;
    Ok(Json(EvidencePageViewModel {
        items,
        total,
        next_cursor,
        scope: "current_projection_window",
    })
    .into_response())
}

async fn load_current_snapshot(
    state: &Arc<ApplicationState>,
) -> Result<ApplicationSnapshot, ApiError> {
    let service = Arc::clone(&state.product_service);
    let cache = Arc::clone(&state.projection_cache);
    tokio::task::spawn_blocking(move || {
        let selection = service.current_selection()?;
        let configuration = ProjectionConfiguration {
            os_run_root: selection.os_run_root,
            semantic_run_root: selection.semantic_run_root,
            mcp_manifest: selection.mcp_manifest,
        };
        load_snapshot(&configuration, &cache)
    })
    .await
    .map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("GUI 查询任务异常结束 error={error}"),
    })?
    .map_err(|message| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message,
    })
}

fn page_bounds(
    cursor: &str,
    limit: usize,
    total: usize,
) -> Result<(usize, usize, Option<String>), ApiError> {
    if !(1..=200).contains(&limit) {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            message: format!("分页 limit 必须位于 1..=200 actual={limit}"),
        });
    }
    let start = cursor.parse::<usize>().map_err(|error| ApiError {
        status: StatusCode::BAD_REQUEST,
        message: format!("分页 cursor 无效 value={cursor} error={error}"),
    })?;
    if start > total {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            message: format!("分页 cursor 超出范围 cursor={start} total={total}"),
        });
    }
    let end = start.saturating_add(limit).min(total);
    let next_cursor = (end < total).then(|| end.to_string());
    Ok((start, end, next_cursor))
}

async fn get_product_settings(
    State(state): State<Arc<ApplicationState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let service = Arc::clone(&state.product_service);
    let settings = tokio::task::spawn_blocking(move || service.settings())
        .await
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("产品设置任务异常结束 error={error}"),
        })?
        .map_err(internal_error)?;
    Ok(Json(settings).into_response())
}

async fn get_retention_plan(
    State(state): State<Arc<ApplicationState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let service = Arc::clone(&state.product_service);
    let plan = tokio::task::spawn_blocking(move || service.retention_plan())
        .await
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("证据保留预览任务异常结束 error={error}"),
        })?
        .map_err(internal_error)?;
    Ok(Json(plan).into_response())
}

async fn delete_expired_observer_runs(
    State(state): State<Arc<ApplicationState>>,
    headers: HeaderMap,
    Json(request): Json<RetentionApplicationRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let service = Arc::clone(&state.product_service);
    let application =
        tokio::task::spawn_blocking(move || service.apply_retention(&request.confirmation_token))
            .await
            .map_err(|error| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message: format!("证据保留执行任务异常结束 error={error}"),
            })?
            .map_err(internal_error)?;
    Ok(Json(application).into_response())
}

async fn post_diagnostic_export(
    State(state): State<Arc<ApplicationState>>,
    headers: HeaderMap,
    Json(request): Json<DiagnosticExportRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let service = Arc::clone(&state.product_service);
    let export = tokio::task::spawn_blocking(move || {
        service.create_diagnostic_export(&request.archive_name)
    })
    .await
    .map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("诊断导出任务异常结束 error={error}"),
    })?
    .map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(export)).into_response())
}

fn authorize(state: &ApplicationState, headers: &HeaderMap) -> Result<(), ApiError> {
    validate_host(state, headers)?;
    if !has_session_cookie(state, headers) {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            message: String::from("GUI API 请求缺少有效本机会话"),
        });
    }
    Ok(())
}

fn internal_error(message: String) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message,
    }
}

fn validate_host(state: &ApplicationState, headers: &HeaderMap) -> Result<(), ApiError> {
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError {
            status: StatusCode::BAD_REQUEST,
            message: String::from("GUI 请求缺少有效 Host 头"),
        })?;
    if host != state.expected_host {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            message: format!(
                "GUI 请求 Host 不匹配 expected={} actual={host}",
                state.expected_host
            ),
        });
    }
    Ok(())
}

fn has_session_cookie(state: &ApplicationState, headers: &HeaderMap) -> bool {
    let expected = format!("agentreins_session={}", state.access_token);
    headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|cookies| cookies.split(';').any(|cookie| cookie.trim() == expected))
}

async fn add_security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let response_headers = response.headers_mut();
    response_headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response_headers.insert(PRAGMA, HeaderValue::from_static("no-cache"));
    response_headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response_headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response_headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    response_headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::page_bounds;

    #[test]
    fn pagination_returns_stable_next_cursor() {
        let first = page_bounds("0", 20, 45).ok().expect("首页应可计算");
        let last = page_bounds("40", 20, 45).ok().expect("尾页应可计算");

        assert_eq!(first, (0, 20, Some(String::from("20"))));
        assert_eq!(last, (40, 45, None));
    }

    #[test]
    fn pagination_rejects_invalid_limit_and_cursor() {
        assert!(page_bounds("0", 0, 10).is_err());
        assert!(page_bounds("invalid", 10, 10).is_err());
        assert!(page_bounds("11", 10, 10).is_err());
    }
}
