//! Static hosting for the web UI.
//!
//! The UI is hand-written HTML and ES modules with no build step and no
//! dependencies, embedded at compile time. That is a deliberate constraint:
//! adding a JavaScript toolchain would make `npm` a prerequisite for building
//! Rook on every platform it targets, and FreeBSD support is exactly where
//! that goes wrong. The browser loads the modules as they are (ADR-0012).

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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn fetch(path: &str) -> (StatusCode, String, String) {
        let response =
            router().oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap_or_default().to_string())
            .unwrap_or_default();
        let bytes = axum::body::to_bytes(response.into_body(), 8 << 20).await.unwrap();
        (status, content_type, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The page is embedded at build time, so a missing or renamed file is a
    /// binary that starts and serves nothing — and nothing else would notice.
    #[tokio::test]
    async fn the_page_is_embedded_in_the_binary() {
        let (status, content_type, body) = fetch("/").await;

        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/html"), "{content_type}");
        assert!(body.contains("<title>"), "the served bytes are not the page");
        assert!(body.contains("/api/chat"), "the page must know where the socket is");
    }

    #[tokio::test]
    async fn an_unknown_path_falls_back_to_the_page_rather_than_a_blank_404() {
        let (status, _, body) = fetch("/sessions/01ABC").await;

        assert_eq!(status, StatusCode::OK, "a client-side route must still load the app");
        assert!(body.contains("<title>"));
    }

    fn embedded_text(name: &str) -> String {
        let file = Assets::get(name).unwrap_or_else(|| panic!("{name} is embedded"));
        String::from_utf8_lossy(&file.data).into_owned()
    }

    /// Every module the page or another module loads must be embedded, or a
    /// browser gets the page back in place of the script — the SPA fallback —
    /// and a blank screen with a syntax error in the console.
    #[tokio::test]
    async fn every_module_the_page_loads_is_embedded_and_served_as_script() {
        let page = embedded_text("index.html");
        let mut wanted: Vec<String> = page
            .split("src=\"/")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .map(String::from)
            .collect();
        assert!(!wanted.is_empty(), "the page loads no module, so the markup moved and this checks nothing");
        let mut seen = std::collections::BTreeSet::new();
        while let Some(name) = wanted.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let (status, content_type, body) = fetch(&format!("/{name}")).await;
            assert_eq!(status, StatusCode::OK, "{name}");
            assert!(content_type.contains("javascript"), "{name} is served as {content_type}");
            assert!(!body.contains("<title>"), "{name} came back as the page: it is not embedded");
            for import in body.split("from './").skip(1).filter_map(|rest| rest.split('\'').next()) {
                wanted.push(import.to_string());
            }
        }
        assert!(seen.len() >= 4, "the modules: {seen:?}");
    }

    /// A tab wired to a renderer that does not exist is a blank screen, and
    /// nothing else notices: the tab list is in the page and the renderers
    /// are in the modules.
    #[test]
    fn every_tab_the_page_offers_has_something_to_draw_it() {
        let page = embedded_text("index.html");
        let modules: String = Assets::iter()
            .filter(|name| name.ends_with(".js"))
            .map(|name| embedded_text(&name))
            .collect::<Vec<_>>()
            .join("\n");

        let tabs: Vec<&str> =
            page.split("data-tab=\"").skip(1).filter_map(|rest| rest.split('"').next()).collect();
        assert!(tabs.len() >= 6, "found {} tabs, so the markup moved and this checks nothing", tabs.len());

        for tab in tabs {
            let named = format!("{tab}: render");
            let at = modules.find(&named).unwrap_or_else(|| panic!("nothing draws the {tab} tab"));
            let renderer: String = modules[at + named.len() - "render".len()..]
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            assert!(
                modules.contains(&format!("function {renderer}(")),
                "the {tab} tab is drawn by {renderer}, which is not defined"
            );
        }
    }
}
