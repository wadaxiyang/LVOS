//! Common synthetic renderer workload, also runnable against the pre-Kit UI.
//! It deliberately uses generated Window show/hide on both revisions. Production
//! no-activate/monitor correctness is measured separately by `ui_window_check`.
use lvos::{MainWindow, QuickLookupPopup, UiRecord};
use slint::{ComponentHandle, LogicalPosition, ModelRc, VecModel, platform::WindowEvent};
use std::{
    cell::Cell,
    error::Error,
    rc::Rc,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn Error>> {
    let start = Instant::now();
    let main = MainWindow::new()?;
    let popup = QuickLookupPopup::new()?;
    let records: Vec<_> = (0..200)
        .map(|i| UiRecord {
            key: format!("fixture-{i}").into(),
            source: format!("synthetic phrase {i}").into(),
            translation: "用于比较同一虚构负载的译文。".repeat(4).into(),
            count: i,
            favorite: false,
            metadata: "Synthetic benchmark".into(),
        })
        .collect();
    main.set_history_records(ModelRc::new(VecModel::from(records.clone())));
    main.set_favorite_records(ModelRc::new(VecModel::from(records)));
    popup.set_source_text("synthetic".into());
    popup.set_translated_text("虚构译文".into());
    popup.set_loading(false);
    let rendered = Rc::new(Cell::new(false));
    let first = rendered.clone();
    main.window().set_rendering_notifier(move |state, _| {
        if matches!(state, slint::RenderingState::AfterRendering) && !first.replace(true) {
            println!(
                "{{\"first_frame_ms\":{}}}",
                start.elapsed().as_secs_f64() * 1000.
            );
        }
    })?;
    main.show()?;
    let timer = slint::Timer::default();
    let index = Cell::new(0_u32);
    let failed = Rc::new(Cell::new(false));
    let failure = failed.clone();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(40),
        move || {
            let i = index.get();
            let result = match i {
                0..=49 | 412..=661 => Ok(()), // settle, then ten seconds idle
                50 => {
                    println!(
                        "{{\"phase\":\"workload\",\"elapsed_ms\":{}}}",
                        start.elapsed().as_millis()
                    );
                    Ok(())
                }
                51..=170 => {
                    main.set_active_page(i32::try_from((i / 10) % 3).unwrap_or(0));
                    main.set_settings_page(i32::try_from((i / 10) % 8).unwrap_or(0));
                    main.window().dispatch_event(WindowEvent::PointerScrolled {
                        position: LogicalPosition::new(450., 380.),
                        delta_x: 0.,
                        delta_y: if i.is_multiple_of(2) { -240. } else { 120. },
                    });
                    Ok(())
                }
                171..=410 => {
                    if i % 2 == 1 {
                        popup.show()
                    } else {
                        popup.hide()
                    }
                } // 120 cycles
                411 => {
                    main.set_active_page(0);
                    println!(
                        "{{\"phase\":\"idle\",\"cycles\":120,\"elapsed_ms\":{}}}",
                        start.elapsed().as_millis()
                    );
                    Ok(())
                }
                _ => {
                    println!(
                        "{{\"phase\":\"complete\",\"elapsed_ms\":{}}}",
                        start.elapsed().as_millis()
                    );
                    let _ = slint::quit_event_loop();
                    Ok(())
                }
            };
            failure.set(failure.get() || result.is_err());
            index.set(i + 1);
        },
    );
    slint::run_event_loop_until_quit()?;
    if !rendered.get() || failed.get() {
        return Err("synthetic renderer workload failed".into());
    }
    Ok(())
}
