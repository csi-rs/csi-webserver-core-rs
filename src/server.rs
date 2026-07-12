//! HTTP server composition: router construction and TCP serving.

use axum::{
    Router,
    routing::{get, post},
};

use crate::routes;
use crate::state::AppState;

/// Bind address for the HTTP server (e.g. `"0.0.0.0:3000"`).
pub struct ServerConfig {
    pub bind: String,
}

/// Build the Axum router with all CSI API routes mounted.
pub fn build_router(state: AppState) -> Router {
    let device_routes = Router::new()
        .route("/config", get(routes::config::get_config))
        .route("/config/reset", post(routes::config::reset_config))
        .route("/config/wifi", post(routes::config::set_wifi))
        .route("/config/traffic", post(routes::config::set_traffic))
        .route("/config/csi", post(routes::config::set_csi))
        .route(
            "/config/collection-mode",
            post(routes::config::set_collection_mode),
        )
        .route("/config/output-mode", post(routes::config::set_output_mode))
        .route("/config/rate", post(routes::config::set_rate))
        .route("/config/protocol", post(routes::config::set_protocol))
        .route("/config/io-tasks", post(routes::config::set_io_tasks))
        .route("/config/csi-delivery", post(routes::config::set_csi_delivery))
        .route("/control/start", post(routes::control::start_collection))
        .route("/control/stop", post(routes::control::stop_collection))
        .route("/control/status", get(routes::control::get_collection_status))
        .route("/control/reset", post(routes::control::reset_esp32))
        .route("/control/stats", post(routes::config::show_stats))
        .route("/info", get(routes::info::get_info))
        .route("/ws", get(routes::ws::ws_handler));

    Router::new()
        .route("/", get(|| async { "CSI Server Active" }))
        .route("/api/devices", get(routes::devices::list_devices))
        .nest("/api/devices/{id}", device_routes)
        .with_state(state)
}

/// Bind `config.bind` and serve until the process exits.
pub async fn serve(config: ServerConfig, state: AppState) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!("CSI server listening on http://{}", config.bind);
    axum::serve(listener, build_router(state)).await
}
