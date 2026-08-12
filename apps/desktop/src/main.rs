use lvos::{DesktopRuntime, SlintUiDispatcher};
use lvos_core::{PRODUCT_NAME, SOFTWARE_VERSION};

fn main() {
    init_tracing();
    tracing::info!(version = SOFTWARE_VERSION, "{PRODUCT_NAME} starting");
    let runtime = DesktopRuntime::new(SlintUiDispatcher);
    runtime.shutdown();
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();
}
