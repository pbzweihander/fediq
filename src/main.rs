mod api;
mod config;
mod handler;
mod internationalization;

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
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let router = crate::handler::create_router();

    let listen_addr = &crate::config::CONFIG.listen_addr;
    tracing::info!(%listen_addr, "starting http server...");
    axum::Server::bind(&listen_addr.parse()?)
        .serve(router.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}
