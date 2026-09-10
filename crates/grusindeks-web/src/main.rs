#![recursion_limit = "512"]

//! Axum server entry point (ssr feature). Wires Leptos SSR + server functions
//! and injects [`AppState`] into request context so server fns can reach the
//! shared MET client and config.

#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::routing::get;
    use axum::Router;
    use grusindeks_web::app::{shell, App};
    use grusindeks_web::state::AppState;
    use leptos::logging::log;
    use leptos::prelude::*;
    use leptos_axum::{generate_route_list_with_exclusions_and_ssg_and_context, LeptosRoutes};
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

    let app_state = AppState::init().await.expect("build application state");
    // App renders the NavBar (and starts its resources) during route discovery
    // and 404 rendering too, not just when serving a matched page.
    let provide_app_state = {
        let app_state = app_state.clone();
        move || provide_context(app_state.clone())
    };
    let (routes, _) = generate_route_list_with_exclusions_and_ssg_and_context(
        App,
        None,
        provide_app_state.clone(),
    );

    let app = Router::new()
        .route(
            "/api/index/today",
            get(grusindeks_web::api::index_today).with_state(app_state.clone()),
        )
        .leptos_routes_with_context(&leptos_options, routes, provide_app_state.clone(), {
            let leptos_options = leptos_options.clone();
            move || shell(leptos_options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler_with_context(
            provide_app_state,
            shell,
        ))
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
