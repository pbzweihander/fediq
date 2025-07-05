mod api;
mod config;
mod handler;
mod internationalization;

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("signal received, starting graceful shutdown");
}

#[tokio::main]
async fn main() {
    color_eyre::install().expect("failed to install color-eyre");

    tracing_subscriber::registry()
        .with(tracing_error::ErrorLayer::default())
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let router = crate::handler::create_router();

    let listen_addr = &crate::config::CONFIG.listen_addr;
    tracing::info!(%listen_addr, "starting http server...");
    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .expect("failed to bind TCP listener");
    axum::serve(listener, router.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("failed to serve HTTP");
}
