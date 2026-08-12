use std::error::Error;

use lvos::{DesktopRuntime, SlintUiDispatcher, UiController};
use lvos_core::{PRODUCT_NAME, SOFTWARE_VERSION};

fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();
    tracing::info!(version = SOFTWARE_VERSION, "{PRODUCT_NAME} starting");
    let runtime = DesktopRuntime::new(SlintUiDispatcher);
    let ui = UiController::new()?;
    ui.show_main_window()?;
    slint::run_event_loop_until_quit()?;
    runtime.shutdown();
    Ok(())
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
