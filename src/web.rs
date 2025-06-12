use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, Json},
    routing::{get, Router},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use tower_http::{cors::CorsLayer, services::ServeDir};
use tracing::info;

use crate::kline::{KLineData, KLineManager};

#[derive(Clone)]
pub struct AppState {
    pub kline_manager: Arc<Mutex<KLineManager>>,
}

#[derive(Serialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub message: Option<String>,
}

#[derive(Serialize)]
pub struct MintInfo {
    pub mint: String,
    pub last_activity: u64,
    pub kline_count: usize,
}

#[derive(Deserialize)]
pub struct KlineQuery {
    pub limit: Option<usize>,
}

pub async fn create_web_server(kline_manager: Arc<Mutex<KLineManager>>) -> Router {
    let state = AppState { kline_manager };

    Router::new()
        .route("/", get(serve_index))
        .route("/api/mints", get(get_mints))
        .route("/api/mint/:mint/klines", get(get_klines))
        .route("/api/stats", get(get_stats))
        .nest_service("/static", ServeDir::new("static"))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn serve_index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

async fn get_mints(State(state): State<AppState>) -> Result<Json<ApiResponse<Vec<MintInfo>>>, StatusCode> {
    let manager = state.kline_manager.lock().await;
    
    match manager.get_active_mints().await {
        Ok(active_mints) => {
            let mut mint_infos = Vec::new();
            
            for (mint, last_activity) in active_mints {
                // Get K-line count for this mint
                match manager.get_klines_for_mint(&mint, None).await {
                    Ok(klines) => {
                        mint_infos.push(MintInfo {
                            mint,
                            last_activity,
                            kline_count: klines.len(),
                        });
                    }
                    Err(_) => {
                        mint_infos.push(MintInfo {
                            mint,
                            last_activity,
                            kline_count: 0,
                        });
                    }
                }
            }
            
            Ok(Json(ApiResponse {
                success: true,
                data: Some(mint_infos),
                message: None,
            }))
        }
        Err(e) => {
            Ok(Json(ApiResponse {
                success: false,
                data: None,
                message: Some(format!("Failed to get mints: {}", e)),
            }))
        }
    }
}

async fn get_klines(
    Path(mint): Path<String>,
    Query(params): Query<KlineQuery>,
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<KLineData>>>, StatusCode> {
    let manager = state.kline_manager.lock().await;
    
    match manager.get_klines_for_mint(&mint, params.limit).await {
        Ok(klines) => {
            Ok(Json(ApiResponse {
                success: true,
                data: Some(klines),
                message: None,
            }))
        }
        Err(e) => {
            Ok(Json(ApiResponse {
                success: false,
                data: None,
                message: Some(format!("Failed to get K-lines: {}", e)),
            }))
        }
    }
}

async fn get_stats(State(state): State<AppState>) -> Result<Json<ApiResponse<HashMap<String, usize>>>, StatusCode> {
    let manager = state.kline_manager.lock().await;
    
    match manager.get_stats().await {
        Ok((mint_count, kline_count)) => {
            let mut stats = HashMap::new();
            stats.insert("total_mints".to_string(), mint_count);
            stats.insert("total_klines".to_string(), kline_count);
            
            Ok(Json(ApiResponse {
                success: true,
                data: Some(stats),
                message: None,
            }))
        }
        Err(e) => {
            Ok(Json(ApiResponse {
                success: false,
                data: None,
                message: Some(format!("Failed to get stats: {}", e)),
            }))
        }
    }
}

pub async fn start_web_server(
    kline_manager: Arc<Mutex<KLineManager>>,
    port: u16,
) -> anyhow::Result<()> {
    let app = create_web_server(kline_manager).await;
    
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    info!("Web server starting on http://0.0.0.0:{}", port);
    
    axum::serve(listener, app).await?;
    
    Ok(())
}
