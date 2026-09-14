#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod ui_ipc;
mod ui_session;

use std::{error::Error, sync::mpsc as std_mpsc, time::Duration};

use lvos::UiProcessCoordinator;
use ui_ipc::{IncomingMessage, UiIpcClient};
use ui_session::UiSession;

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().any(|argument| argument == "--ui-smoke") {
        return ui_smoke();
    }
    init_tracing();
    tracing::info!(
        version = lvos_core::SOFTWARE_VERSION,
        process_id = std::process::id(),
        process = "ui",
        "LVOS UI starting"
    );

    let ui = UiProcessCoordinator::new()?;
    let (incoming_tx, incoming_rx) = std_mpsc::channel::<IncomingMessage>();
    let ipc = UiIpcClient::start(incoming_tx)?;
    let session = UiSession::install(ui, ipc.requests(), ipc.generation());
    let pump = slint::Timer::default();
    pump.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(8),
        move || {
            while let Ok(message) = incoming_rx.try_recv() {
                session.apply(message.sequence, message.payload);
                if let Some(applied) = message.applied {
                    let _ = applied.send(());
                }
            }
        },
    );

    slint::run_event_loop_until_quit()?;
    ipc.announce_exit();
    drop(pump);
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn ui_smoke() -> Result<(), Box<dyn Error>> {
    use slint::ComponentHandle;

    let ui = UiProcessCoordinator::new()?;
    let frames = std::rc::Rc::new(std::cell::Cell::new(0_u8));
    for (index, window) in [
        ui.main_window().window(),
        ui.popup().window(),
        ui.permission_window().window(),
    ]
    .into_iter()
    .enumerate()
    {
        let frames = std::rc::Rc::clone(&frames);
        window.set_rendering_notifier(move |state, _| {
            if matches!(state, slint::RenderingState::AfterRendering) {
                frames.set(frames.get() | (1 << index));
            }
        })?;
    }
    ui.main_window().set_active_page(2);
    ui.show_main_window()?;
    ui.show_lookup_card(&lvos::LookupCardState::Loading {
        generation: 1,
        source: "Synthetic package verification".to_owned(),
    })?;
    ui.permission_window().show()?;
    slint::Timer::single_shot(Duration::from_secs(3), || {
        let _ = slint::quit_event_loop();
    });
    slint::run_event_loop_until_quit()?;
    if frames.get() != 0b111 {
        return Err("packaged UI smoke: not all three windows rendered".into());
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn ui_smoke() -> Result<(), Box<dyn Error>> {
    Err("packaged UI smoke requires a supported desktop host".into())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
}
