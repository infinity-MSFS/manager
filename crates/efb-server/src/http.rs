//! Plain HTTP routes: health, code redemption and the image routes.
//!
//! Images go over HTTP rather than the socket because the socket is JSON
//! text, re-serialised once per client, inside the sim's JS engine.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tracing::debug;

use crate::pairing::Redeem;
use crate::providers::{MAX_TILE_ZOOM, TILE_LAYERS};
use crate::server::Ctx;

const IMAGE_CACHE: &str = "private, max-age=300";

pub(crate) async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

#[derive(Deserialize)]
pub(crate) struct PairParams {
    code: Option<String>,
}

/// Trades a short code for its token, once. Open: the code is the secret.
pub(crate) async fn pair_redeem(
    Query(params): Query<PairParams>,
    State(ctx): State<Ctx>,
) -> Response {
    let Some(code) = params.code else {
        return (StatusCode::BAD_REQUEST, "missing code").into_response();
    };
    match ctx.shared.codes.lock().await.redeem(&code) {
        Redeem::Token(token) => Json(json!({ "token": token })).into_response(),
        Redeem::Unknown => (StatusCode::NOT_FOUND, "unknown code").into_response(),
        Redeem::Expired => (StatusCode::GONE, "code expired").into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct TokenParam {
    token: Option<String>,
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "pairing token required").into_response()
}

fn image(content_type: &'static str, cache: &'static str, bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache),
        ],
        bytes,
    )
        .into_response()
}

/// `{n}` is 1-based and may carry an extension (`3.jpg`).
pub(crate) async fn doc_page(
    State(ctx): State<Ctx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(params): Query<TokenParam>,
    Path((id, page)): Path<(String, String)>,
) -> Response {
    if ctx.authorize(peer, params.token.as_deref()).await.is_none() {
        return unauthorized();
    }
    let Some(docs) = ctx.providers.docs.as_ref() else {
        return (StatusCode::NOT_FOUND, "no document provider").into_response();
    };
    let n = match strip_ext(&page).parse::<usize>() {
        Ok(0) => return (StatusCode::BAD_REQUEST, "page is 1-based").into_response(),
        Ok(n) => n,
        Err(_) => return (StatusCode::BAD_REQUEST, "bad page number").into_response(),
    };
    let path = match docs.page(&id, n - 1).await {
        Ok(p) => p,
        Err(e) => {
            debug!(id, page = n, error = %e, "efb-server: page render failed");
            return (StatusCode::NOT_FOUND, "page unavailable").into_response();
        }
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => image("image/jpeg", IMAGE_CACHE, bytes),
        Err(_) => (StatusCode::NOT_FOUND, "page not rendered").into_response(),
    }
}

pub(crate) async fn nav_tile(
    State(ctx): State<Ctx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(params): Query<TokenParam>,
    Path((layer, z, x, y)): Path<(String, String, String, String)>,
) -> Response {
    if ctx.authorize(peer, params.token.as_deref()).await.is_none() {
        return unauthorized();
    }
    let Some(tiles) = ctx.providers.tiles.as_ref() else {
        return (StatusCode::NOT_FOUND, "no tile source").into_response();
    };
    if !TILE_LAYERS.contains(&layer.as_str()) {
        return (StatusCode::BAD_REQUEST, "unknown layer").into_response();
    }
    let (Ok(z), Ok(x), Ok(y)) = (
        z.parse::<u8>(),
        x.parse::<u32>(),
        strip_ext(&y).parse::<u32>(),
    ) else {
        return (StatusCode::BAD_REQUEST, "bad tile coords").into_response();
    };
    if z > MAX_TILE_ZOOM {
        return (StatusCode::BAD_REQUEST, "zoom out of range").into_response();
    }
    match tiles.tile(&layer, z, x, y).await {
        Ok(bytes) => image("image/png", IMAGE_CACHE, bytes),
        Err(e) => {
            debug!(layer, z, x, y, error = %e, "efb-server: nav tile failed");
            (StatusCode::NOT_FOUND, "tile unavailable").into_response()
        }
    }
}

pub(crate) async fn nav_chart(
    State(ctx): State<Ctx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(params): Query<TokenParam>,
    Path((icao, file)): Path<(String, String)>,
) -> Response {
    if ctx.authorize(peer, params.token.as_deref()).await.is_none() {
        return unauthorized();
    }
    let Some(nav) = ctx.providers.nav.as_ref() else {
        return (StatusCode::NOT_FOUND, "no navigraph provider").into_response();
    };
    match nav.chart(&icao, &file).await {
        // Licensed imagery: nothing keeps a copy.
        Ok(bytes) => image("image/png", "no-store", bytes),
        Err(e) => {
            debug!(icao, file, error = %e, "efb-server: nav chart failed");
            (StatusCode::NOT_FOUND, "chart unavailable").into_response()
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct FrameParams {
    token: Option<String>,
    seq: Option<u64>,
}

/// `no-store` stops a proxy holding a frame; the changing `seq` in the URL is
/// what makes an `<img>` refetch at all.
pub(crate) async fn browser_frame(
    State(ctx): State<Ctx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(params): Query<FrameParams>,
    Path(session): Path<String>,
) -> Response {
    if ctx.authorize(peer, params.token.as_deref()).await.is_none() {
        return unauthorized();
    }
    let Some(browser) = ctx.providers.browser.as_ref() else {
        return (StatusCode::NOT_FOUND, "no browser provider").into_response();
    };
    let Some(seq) = params.seq else {
        return (StatusCode::BAD_REQUEST, "seq required").into_response();
    };
    match browser.frame(&session, seq).await {
        Some(bytes) => image("image/jpeg", "no-store", bytes),
        None => (StatusCode::NOT_FOUND, "frame unavailable").into_response(),
    }
}

fn strip_ext(s: &str) -> &str {
    s.split('.').next().unwrap_or(s)
}
