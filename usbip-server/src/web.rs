use std::{net::SocketAddr, sync::Arc};

use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    response::sse::{Event, Sse},
    routing::{get, post},
    Json, Router,
};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use serde::{Deserialize, Serialize};
use tower_http::trace::TraceLayer;

use crate::{
    rules::{best_effort_rule_for_device, Rule, RuleStore, RulesFile, SharedRules},
    usb::UsbIp,
};

/// Web 层职责：
/// - 提供设备视图：udev 枚举 + `usbip` 状态（exported / port 占用）
/// - 提供“手动 bind/unbind”的 API，并把这个手动操作同步为持久化规则（下次自动恢复）
/// - 提供“应用规则”的入口（用于启动后/调试时快速对齐状态）
#[derive(Clone)]
pub struct AppState {
    pub rules: SharedRules,
    pub rules_store: Arc<RuleStore>,
    pub usb: UsbIp,
    pub events: broadcast::Sender<String>,
}

pub async fn serve(listen: SocketAddr, state: AppState) -> Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/events", get(api_events))
        .route("/api/devices", get(api_devices))
        .route("/api/rules", get(api_get_rules).put(api_put_rules))
        .route("/api/bind", post(api_bind))
        .route("/api/unbind", post(api_unbind))
        .route("/api/rules/add_for_busid", post(api_add_rule_for_busid))
        .route("/api/rules/toggle", post(api_rules_toggle))
        .route("/api/rules/delete", post(api_rules_delete))
        .route("/api/apply", post(api_apply))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(%listen, "web listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

static INDEX_HTML: &str = include_str!("../assets/index.html");

#[derive(Debug, Clone, Serialize)]
struct DeviceView {
    busid: String,
    serial: Option<String>,
    devpath: Option<String>,
    vid: Option<u16>,
    pid: Option<u16>,
    manufacturer: Option<String>,
    product: Option<String>,
    exported: bool,
    in_use: bool,
    remote: Option<String>,
}

async fn api_devices(State(st): State<AppState>) -> Result<Json<Vec<DeviceView>>, ApiError> {
    let mut devices = st.usb.list_devices().await.map_err(ApiError::internal)?;
    let exported = st.usb.exported_busids().await.unwrap_or_default();
    let ports = st.usb.port_status().await.unwrap_or_default();

    // 过滤不可绑定/噪声设备：
    // - busid 不包含 '-' 的一般是 root hub/bus 节点
    // - device_class=0x09 是 USB Hub（通常没有意义且会干扰 UI）
    devices.retain(|d| d.busid.contains('-') && d.device_class != Some(0x09));

    let out = devices
        .into_iter()
        .map(|d| DeviceView {
            exported: exported.contains(&d.busid),
            in_use: ports.get(&d.busid).map(|p| p.in_use).unwrap_or(false),
            remote: ports.get(&d.busid).and_then(|p| p.remote.clone()),
            busid: d.busid,
            serial: d.serial,
            devpath: d.devpath,
            vid: d.vid,
            pid: d.pid,
            manufacturer: d.manufacturer,
            product: d.product,
        })
        .collect();
    Ok(Json(out))
}

async fn api_get_rules(State(st): State<AppState>) -> Result<Json<RulesFile>, ApiError> {
    let rules = st.rules.read().await.clone();
    Ok(Json(rules))
}

async fn api_put_rules(
    State(st): State<AppState>,
    Json(new_rules): Json<RulesFile>,
) -> Result<Json<RulesFile>, ApiError> {
    st.rules_store
        .store(&new_rules)
        .await
        .map_err(ApiError::internal)?;
    *st.rules.write().await = new_rules.clone();
    let _ = st.events.send("rules".to_string());
    Ok(Json(new_rules))
}

#[derive(Debug, Clone, Deserialize)]
struct BindReq {
    busid: String,
}

async fn api_bind(State(st): State<AppState>, Json(req): Json<BindReq>) -> Result<(), ApiError> {
    st.usb.bind(&req.busid).await.map_err(ApiError::internal)?;

    // Persist "manual bind intent" as a rule so next plug-in auto-binds.
    // 关键：手动 bind 不应是“临时状态”，应变成规则，这样重启/热插拔后还能自动恢复。
    persist_manual_rule(&st, &req.busid, true).await?;
    let _ = st.events.send("bind".to_string());
    Ok(())
}

async fn api_unbind(State(st): State<AppState>, Json(req): Json<BindReq>) -> Result<(), ApiError> {
    st.usb
        .unbind(&req.busid)
        .await
        .map_err(ApiError::internal)?;

    // Persist "manual unbind intent" by disabling the matching rule (if any).
    // 注意：我们不删除规则，而是禁用它，便于之后重新启用/排查。
    persist_manual_rule(&st, &req.busid, false).await?;
    let _ = st.events.send("unbind".to_string());
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct AddRuleReq {
    busid: String,
    note: Option<String>,
}

async fn api_add_rule_for_busid(
    State(st): State<AppState>,
    Json(req): Json<AddRuleReq>,
) -> Result<Json<RulesFile>, ApiError> {
    let devices = st.usb.list_devices().await.map_err(ApiError::internal)?;
    let Some(d) = devices.into_iter().find(|d| d.busid == req.busid) else {
        return Err(ApiError::bad_request("device not found"));
    };
    let Some(match_spec) = best_effort_rule_for_device(&d) else {
        return Err(ApiError::bad_request("device has no stable identifiers"));
    };
    let rule = Rule::new(match_spec, req.note).map_err(ApiError::bad_request)?;

    let mut rf = st.rules.write().await;
    rf.rules.push(rule);
    st.rules_store
        .store(&rf)
        .await
        .map_err(ApiError::internal)?;
    let _ = st.events.send("rules".to_string());
    Ok(Json(rf.clone()))
}

#[derive(Debug, Clone, Deserialize)]
struct ToggleRuleReq {
    id: uuid::Uuid,
    enabled: bool,
}

async fn api_rules_toggle(
    State(st): State<AppState>,
    Json(req): Json<ToggleRuleReq>,
) -> Result<Json<RulesFile>, ApiError> {
    let mut rf = st.rules.write().await;
    let Some(r) = rf.rules.iter_mut().find(|r| r.id == req.id) else {
        return Err(ApiError::bad_request("rule not found"));
    };
    r.enabled = req.enabled;
    st.rules_store
        .store(&rf)
        .await
        .map_err(ApiError::internal)?;
    let _ = st.events.send("rules".to_string());
    Ok(Json(rf.clone()))
}

#[derive(Debug, Clone, Deserialize)]
struct DeleteRuleReq {
    id: uuid::Uuid,
}

async fn api_rules_delete(
    State(st): State<AppState>,
    Json(req): Json<DeleteRuleReq>,
) -> Result<Json<RulesFile>, ApiError> {
    let mut rf = st.rules.write().await;
    let before = rf.rules.len();
    rf.rules.retain(|r| r.id != req.id);
    if rf.rules.len() == before {
        return Err(ApiError::bad_request("rule not found"));
    }
    st.rules_store
        .store(&rf)
        .await
        .map_err(ApiError::internal)?;
    let _ = st.events.send("rules".to_string());
    Ok(Json(rf.clone()))
}

async fn api_apply(State(st): State<AppState>) -> Result<(), ApiError> {
    let rules = st.rules.read().await.clone();
    st.usb
        .apply_rules(&rules, &st.rules_store)
        .await
        .map_err(ApiError::internal)?;
    let _ = st.events.send("apply".to_string());
    Ok(())
}

async fn api_events(State(st): State<AppState>) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = st.events.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| {
        match msg {
            Ok(v) => Some(Ok(Event::default().event("update").data(v))),
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

async fn persist_manual_rule(st: &AppState, busid: &str, enabled: bool) -> Result<(), ApiError> {
    let devices = st.usb.list_devices().await.map_err(ApiError::internal)?;
    let Some(d) = devices.into_iter().find(|d| d.busid == busid) else {
        return Err(ApiError::bad_request("device not found"));
    };
    let Some(match_spec) = best_effort_rule_for_device(&d) else {
        return Err(ApiError::bad_request("device has no stable identifiers"));
    };

    let mut rf = st.rules.write().await;
    let note = Some("manual".to_string());
    rf.upsert_bind_rule(match_spec.clone(), enabled, note)
        .map_err(ApiError::bad_request)?;
    if !enabled {
        rf.disable_rule(&match_spec);
    }
    st.rules_store
        .store(&rf)
        .await
        .map_err(ApiError::internal)?;
    Ok(())
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    msg: String,
}

impl ApiError {
    fn bad_request<E: ToString>(e: E) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            msg: e.to_string(),
        }
    }

    fn internal<E: ToString>(e: E) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            msg: e.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.status, self.msg).into_response()
    }
}
