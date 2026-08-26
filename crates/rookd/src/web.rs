//! Static hosting for the web UI.
//!
//! The UI is a single hand-written HTML file with no build step and no
//! dependencies. That is a deliberate constraint: adding a JavaScript toolchain
//! would make `npm` a prerequisite for building Rook on every platform it
//! targets, and FreeBSD support is exactly where that goes wrong.

use axum::Router;
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct Assets;

pub fn router() -> Router {
    Router::new().route("/", get(index)).fallback(static_handler)
}

async fn index() -> Response {
    serve("index.html")
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.is_empty() {
        return serve("index.html");
    }
    if Assets::get(path).is_some() {
        return serve(path);
    }
    // Unknown paths under /api are a genuine 404; anything else is the SPA.
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "no such endpoint").into_response();
    }
    serve("index.html")
}

fn serve(path: &str) -> Response {
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
