use tokio_util::sync::CancellationToken;
use tracing::warn;

pub async fn listen_shutdown(token: CancellationToken) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(err) => {
                warn!("failed to install SIGTERM handler: error={err}");
                if tokio::signal::ctrl_c().await.is_ok() {
                    warn!("shutdown signal received: SIGINT");
                    token.cancel();
                }
                return;
            }
        };

        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(stream) => stream,
            Err(err) => {
                warn!("failed to install SIGINT handler: error={err}");
                if tokio::signal::ctrl_c().await.is_ok() {
                    warn!("shutdown signal received: SIGINT");
                    token.cancel();
                }
                return;
            }
        };

        tokio::select! {
            _ = sigterm.recv() => warn!("shutdown signal received: SIGTERM"),
            _ = sigint.recv() => warn!("shutdown signal received: SIGINT"),
        }

        token.cancel();
        return;
    }

    #[cfg(not(unix))]
    if tokio::signal::ctrl_c().await.is_ok() {
        warn!("shutdown signal received: SIGINT");
        token.cancel();
    }
}
