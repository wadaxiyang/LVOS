//! Exercise the real Kit modal and LVOS broker with inert business requests.
use lvos::{ConfirmationBroker, ConfirmationRequest, DeviceRecord, UiController};
use slint::{
    ComponentHandle, LogicalPosition, SharedString,
    platform::{Key, PointerEventButton, WindowEvent},
};
use std::{
    cell::{Cell, RefCell},
    error::Error,
    rc::Rc,
    time::Duration,
};

fn key(window: &slint::Window, text: SharedString) {
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}
fn click(window: &slint::Window, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    let button = PointerEventButton::Left;
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button });
    window.dispatch_event(WindowEvent::PointerReleased { position, button });
}

#[derive(Default)]
struct Results {
    requested: Cell<u32>,
    decisions: RefCell<Vec<bool>>,
    dispatch_failed: Cell<bool>,
}

fn request(broker: ConfirmationBroker, target: i32, results: Rc<Results>) {
    results.requested.set(results.requested.get() + 1);
    let epoch = broker.epoch();
    let failure = Rc::clone(&results);
    if slint::spawn_local(async move {
        let decision = broker.confirm(epoch, ConfirmationRequest {
            title: "Synthetic confirmation".into(),
            message: "No database, account, or device will change. This verifies the real confirmation and cancellation path.".into(),
            primary: "Confirm".into(), focus_target: target, device_id: "fixture-device".into(),
        }).await;
        results.decisions.borrow_mut().push(decision);
    }).is_err() { failure.dispatch_failed.set(true); }
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let ui = Rc::new(UiController::new()?);
    ui.main_window()
        .set_reduce_motion(std::env::args().any(|arg| arg == "reduced"));
    let results = Rc::new(Results::default());
    let broker = ui.confirmations().clone();
    let r = Rc::clone(&results);
    ui.main_window()
        .on_import_data_requested(move || request(broker.clone(), 3, Rc::clone(&r)));
    let broker = ui.confirmations().clone();
    let r = Rc::clone(&results);
    ui.main_window()
        .on_regenerate_device_identity_requested(move || request(broker.clone(), 2, Rc::clone(&r)));
    let broker = ui.confirmations().clone();
    let r = Rc::clone(&results);
    ui.main_window()
        .on_revoke_device_requested(move |_| request(broker.clone(), 1, Rc::clone(&r)));
    ui.set_devices(vec![DeviceRecord {
        id: "fixture-device".into(),
        name: "Synthetic device".into(),
        platform: "Windows".into(),
        last_seen: "Fixture".into(),
        current: true,
        revoked: false,
    }]);
    ui.main_window().set_active_page(2);
    ui.main_window().set_settings_page(6);
    ui.show_main_window()?;
    let failed = Rc::new(Cell::new(false));
    let failure = Rc::clone(&failed);
    let index = Cell::new(0);
    let restore_count = Cell::new(0);
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(350),
        move || {
            let main = ui.main_window();
            let window = main.window();
            let (name, ok) = match index.get() {
                0 => {
                    click(window, 740., 280.);
                    ("real Import trigger", results.requested.get() == 1)
                }
                1 => {
                    let shown = main.get_confirmation_shown() && main.get_confirmation_blocking();
                    for _ in 0..9 {
                        key(window, Key::Tab.into());
                    }
                    window.dispatch_event(WindowEvent::KeyPressed {
                        text: Key::Shift.into(),
                    });
                    key(window, Key::Tab.into());
                    window.dispatch_event(WindowEvent::KeyReleased {
                        text: Key::Shift.into(),
                    });
                    click(window, 60., 64.);
                    let isolated = main.get_active_page() == 2 && results.requested.get() == 1;
                    key(window, Key::Escape.into());
                    (
                        "modal traps keys and blocks background navigation",
                        shown && isolated,
                    )
                }
                2 => {
                    let cancelled =
                        !main.get_confirmation_blocking() && *results.decisions.borrow() == [false];
                    key(window, Key::Return.into());
                    (
                        "Escape cancels and restores Import focus",
                        cancelled && results.requested.get() == 2,
                    )
                }
                3 => {
                    key(window, Key::Tab.into());
                    key(window, Key::Return.into());
                    key(window, Key::Return.into());
                    ("accept requested", true)
                }
                4 => {
                    let once = *results.decisions.borrow() == [false, true];
                    main.invoke_import_data_requested();
                    main.invoke_import_data_requested();
                    ("one accept despite repeated Enter", once)
                }
                5 => {
                    let duplicate = main.get_confirmation_shown()
                        && results.decisions.borrow().len() == 3
                        && !results.decisions.borrow()[2];
                    ui.confirmations().invalidate();
                    ("duplicate cannot replace the pending request", duplicate)
                }
                6 => {
                    let cancelled =
                        !main.get_confirmation_blocking() && results.decisions.borrow().len() == 4;
                    main.invoke_import_data_requested();
                    ("context change cancels pending", cancelled)
                }
                7 => {
                    let ok = ui.hide_main_window().is_ok();
                    ("hide management window", ok)
                }
                8 => {
                    let cancelled =
                        !window.is_visible() && results.decisions.borrow().last() == Some(&false);
                    let _ = ui.show_main_window();
                    main.set_settings_page(2);
                    ("hidden window cancels pending", cancelled)
                }
                9 => {
                    main.invoke_regenerate_device_identity_requested();
                    ("identity request", true)
                }
                10 => {
                    let shown =
                        main.get_confirmation_shown() && main.get_confirmation_focus_target() == 2;
                    key(window, Key::Escape.into());
                    ("identity modal", shown)
                }
                11 => {
                    restore_count.set(results.requested.get());
                    key(window, Key::Return.into());
                    (
                        "identity trigger focus restored",
                        results.requested.get() == restore_count.get() + 1,
                    )
                }
                12 => {
                    key(window, Key::Escape.into());
                    ("cancel identity again", true)
                }
                13 => {
                    main.set_settings_page(4);
                    ("locate Devices", true)
                }
                14 => {
                    main.invoke_revoke_device_requested("fixture-device".into());
                    ("device request", true)
                }
                15 => {
                    let shown =
                        main.get_confirmation_shown() && main.get_confirmation_focus_target() == 1;
                    key(window, Key::Escape.into());
                    ("device modal", shown)
                }
                16 => {
                    restore_count.set(results.requested.get());
                    key(window, Key::Return.into());
                    (
                        "exact device trigger focus restored",
                        results.requested.get() == restore_count.get() + 1,
                    )
                }
                17 => {
                    key(window, Key::Escape.into());
                    ("cancel device again", true)
                }
                18 => {
                    main.invoke_import_data_requested();
                    ("request before closing window", true)
                }
                19 => {
                    window.dispatch_event(WindowEvent::CloseRequested);
                    ("native close request", true)
                }
                _ => (
                    "close cancels without accepting business work",
                    !window.is_visible()
                        && !main.get_confirmation_shown()
                        && results
                            .decisions
                            .borrow()
                            .iter()
                            .filter(|accepted| **accepted)
                            .count()
                            == 1,
                ),
            };
            let ok = ok && !results.dispatch_failed.get();
            println!(
                "{} {}: {name}",
                if ok { "PASS" } else { "FAIL" },
                index.get()
            );
            failure.set(failure.get() || !ok);
            if index.get() == 20 {
                let _ = slint::quit_event_loop();
            }
            index.set(index.get() + 1);
        },
    );
    slint::run_event_loop_until_quit()?;
    if failed.get() {
        return Err("modal interaction check failed".into());
    }
    Ok(())
}
