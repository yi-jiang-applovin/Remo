//! A zero-install way to reach Track A (`Remo.invoke`/`Remo.listCapabilities`)
//! from a plain browser tab — no `remo-cli` download, no MCP server.
//!
//! Track B (the standard `Page`/`DOM`/`CSS`/`Overlay` domains) already needs
//! nothing but Chrome itself (`chrome://inspect` or a pasted `devtools://`
//! URL) — Chrome's own frontend speaks those domains natively. Track A has no
//! such built-in client: Chrome's UI doesn't know how to call a custom
//! `Remo.*` domain. Before this module, reaching it required either
//! `remo-cli`, `remo-mcp`, or hand-rolling a WebSocket client.
//!
//! This serves one self-contained HTML page — inline CSS/JS, no external
//! requests — at `/console`, over the same plain `http://` origin the target
//! is already listening on. It does two things at once:
//!
//! - A minimal human UI: list capabilities, invoke one with JSON params, see
//!   the result.
//! - A `window.remo` JS object (`listCapabilities()`, `invoke(name, args)`,
//!   `call(method, params)` for the raw CDP escape hatch) — the point of
//!   exposing it as a plain object, not just wiring the UI internally, is
//!   that *any* browser-automation tool an agent already has (Playwright,
//!   chrome-devtools-mcp, etc.) can drive it with nothing more than
//!   "navigate here, then evaluate JS" — no CLI, no extra MCP server to
//!   install. The page also logs a one-line usage hint to the console on
//!   load so an agent that lands here cold (via `browser_console_messages`
//!   or equivalent) can self-discover the API without external docs.

use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use axum::Router;

pub fn router() -> Router {
    Router::new()
        .route("/console", get(page))
        // Browsers request this unconditionally; without a handler it's a
        // 404 that shows up as console noise on every page load — harmless,
        // but distracting when using the console's own JS console output as
        // the "how do I use this" documentation (see the crate-level docs).
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
}

async fn page() -> Html<&'static str> {
    Html(PAGE)
}

const PAGE: &str = include_str!("console.html");

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_a_page_exposing_window_remo() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/console")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            html.contains("window.remo"),
            "page should expose window.remo"
        );
        assert!(
            html.contains("Remo.invoke"),
            "page should document the Remo.invoke usage path"
        );
    }

    #[tokio::test]
    async fn favicon_is_a_quiet_204_not_a_noisy_404() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/favicon.ico")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}
