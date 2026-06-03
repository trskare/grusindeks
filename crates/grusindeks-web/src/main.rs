//! Axum server entry point (ssr feature). Wires Leptos SSR + server functions
//! and injects [`AppState`] into request context so server fns can reach the
//! shared MET client and config.

#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::Router;
    use grusindeks_web::app::{shell, App};
    use grusindeks_web::state::AppState;
    use leptos::logging::log;
    use leptos::prelude::*;
    use leptos_axum::{generate_route_list, LeptosRoutes};
    use tower_http::compression::CompressionLayer;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let conf = get_configuration(None).expect("read leptos configuration");
    let leptos_options = conf.leptos_options;
    let addr = leptos_options.site_addr;
    let routes = generate_route_list(App);

    let app_state = AppState::init().await.expect("build application state");

    let app = Router::new()
        .leptos_routes_with_context(
            &leptos_options,
            routes,
            {
                let app_state = app_state.clone();
                move || provide_context(app_state.clone())
            },
            {
                let leptos_options = leptos_options.clone();
                move || shell(leptos_options.clone())
            },
        )
        .fallback(leptos_axum::file_and_error_handler(shell))
        .layer(CompressionLayer::new())
        .with_state(leptos_options);

    log!("grusindeks-web listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind listener");
    axum::serve(listener, app.into_make_service())
        .await
        .expect("serve");
}

// Building for wasm (hydrate) or with no features: the binary is a no-op.
#[cfg(not(feature = "ssr"))]
fn main() {}
