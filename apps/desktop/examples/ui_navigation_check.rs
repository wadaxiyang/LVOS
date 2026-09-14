//! Native UI checks with generated components, synthetic data, and inert callbacks.
//! Run separately: `cargo run --locked -p lvos --example ui_navigation_check`.
use lvos::{DeviceRecord, FeedbackKind, MainWindow, UiRecord};
use slint::{
    ComponentHandle, LogicalPosition, LogicalSize, SharedString,
    platform::{Key, PointerEventButton, WindowEvent},
};
use std::{cell::Cell, error::Error, rc::Rc, time::Duration};

fn click(ui: &MainWindow, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    let button = PointerEventButton::Left;
    for event in [
        WindowEvent::PointerMoved { position },
        WindowEvent::PointerPressed { position, button },
        WindowEvent::PointerReleased { position, button },
    ] {
        ui.window().dispatch_event(event);
    }
}
fn key(ui: &MainWindow, text: SharedString) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}
fn section(ui: &MainWindow, index: i32) -> bool {
    ui.set_settings_error(SharedString::default());
    ui.set_settings_page(index);
    true
}
#[derive(Default)]
struct Signals {
    scroll: Cell<f32>,
    history_search: Cell<u32>,
    favorites_search: Cell<u32>,
    favorite_state: Cell<Option<bool>>,
    reject_provider: Cell<bool>,
    provider_tests: Cell<u32>,
    provider_saves: Cell<u32>,
    provider_key_empty: Cell<bool>,
    provider_proxy_saved: Cell<bool>,
    password_empty: Cell<bool>,
    logins: Cell<u32>,
    revoked: Cell<u32>,
    revoked_expected_id: Cell<bool>,
    imports: Cell<u32>,
    exports: Cell<u32>,
    network_args_valid: Cell<bool>,
    updates: Cell<u32>,
    startup_calls: Cell<u32>,
    clear_calls: Cell<u32>,
    shortcut_partial: Cell<bool>,
}
fn connect(ui: &MainWindow, signals: &Rc<Signals>) {
    let s = Rc::clone(signals);
    ui.on_history_search(move |_| s.history_search.set(s.history_search.get() + 1));
    let s = Rc::clone(signals);
    ui.on_favorites_search(move |_| s.favorites_search.set(s.favorites_search.get() + 1));
    let s = Rc::clone(signals);
    ui.on_favorite_toggled(move |_, current| s.favorite_state.set(Some(current)));
    let s = Rc::clone(signals);
    ui.on_validate_provider_settings(move |_, _| {
        if s.reject_provider.get() {
            "Fixture validation error".into()
        } else {
            SharedString::default()
        }
    });
    let s = Rc::clone(signals);
    let weak = ui.as_weak();
    ui.on_persist_provider_settings(move |_, _, proxy| {
        s.provider_saves.set(s.provider_saves.get() + 1);
        s.provider_proxy_saved.set(proxy);
        if let Some(ui) = weak.upgrade() {
            ui.set_settings_feedback_kind(FeedbackKind::Success);
            ui.set_settings_error("Provider settings saved.".into());
        }
    });
    let s = Rc::clone(signals);
    ui.on_test_provider(move |_, value| {
        s.provider_tests.set(s.provider_tests.get() + 1);
        s.provider_key_empty.set(value.is_empty());
    });
    let s = Rc::clone(signals);
    ui.on_login_requested(move |_, _, value| {
        s.logins.set(s.logins.get() + 1);
        s.password_empty.set(value.is_empty());
    });
    let s = Rc::clone(signals);
    ui.on_revoke_device_requested(move |id| {
        s.revoked.set(s.revoked.get() + 1);
        s.revoked_expected_id.set(id == "fixture-device");
    });
    let s = Rc::clone(signals);
    ui.on_import_data_requested(move || s.imports.set(s.imports.get() + 1));
    let s = Rc::clone(signals);
    ui.on_export_data_requested(move || s.exports.set(s.exports.get() + 1));
    let s = Rc::clone(signals);
    ui.on_persist_network_settings(move |address, kind, provider, update| {
        s.network_args_valid
            .set(address == "127.0.0.1:7890" && kind == 1 && provider && update);
        SharedString::default()
    });
    let s = Rc::clone(signals);
    ui.on_check_update_requested(move || s.updates.set(s.updates.get() + 1));
    let s = Rc::clone(signals);
    ui.on_update_start_at_login(move |_| {
        s.startup_calls.set(s.startup_calls.get() + 1);
        "Fixture startup error".into()
    });
    let s = Rc::clone(signals);
    ui.on_clear_history_requested(move || s.clear_calls.set(s.clear_calls.get() + 1));
    ui.on_update_launch_minimized(|_| SharedString::default());
    let s = Rc::clone(signals);
    let weak = ui.as_weak();
    ui.on_update_global_hotkey(move |display| {
        if s.shortcut_partial.get() {
            if let Some(ui) = weak.upgrade() {
                ui.set_global_hotkey(display);
            }
            "Fixture shortcut preference error".into()
        } else {
            "Fixture shortcut error".into()
        }
    });
}
type Step = (&'static str, Box<dyn Fn(&MainWindow, &Signals) -> bool>);
fn step(name: &'static str, action: impl Fn(&MainWindow, &Signals) -> bool + 'static) -> Step {
    (name, Box::new(action))
}

#[allow(clippy::too_many_lines)] // Keep the ordered scenario together, with one frame per action.
fn steps() -> Vec<Step> {
    let mut steps = vec![
        step("primary Favorites click", |ui, _| {
            click(ui, 70., 104.);
            ui.get_active_page() == 1
        }),
        step("host accepts pane toggle", |ui, _| {
            click(ui, 25., 22.);
            ui.get_sidebar_collapsed()
        }),
        step("compact History click", |ui, _| {
            click(ui, 25., 64.);
            ui.get_active_page() == 0
        }),
        step("focus movement keeps selection", |ui, _| {
            key(ui, Key::DownArrow.into());
            ui.get_active_page() == 0
        }),
        step("keyboard activates Favorites", |ui, _| {
            key(ui, Key::Return.into());
            ui.get_active_page() == 1
        }),
        step("keyboard reaches footer", |ui, _| {
            key(ui, Key::DownArrow.into());
            key(ui, Key::Return.into());
            ui.get_active_page() == 2
        }),
        step("host expands pane", |ui, _| {
            click(ui, 25., 22.);
            !ui.get_sidebar_collapsed()
        }),
    ];
    for index in 0..8 {
        steps.push(step("request settings section", move |ui, _| {
            section(ui, index)
        }));
        steps.push(step("index locates successive content", move |ui, s| {
            let y = ui.get_settings_scroll_y();
            let located = if index == 0 {
                y.abs() < 1.
            } else {
                y < s.scroll.get() - 1.
            };
            s.scroll.set(y);
            located && ui.get_active_page() == 2 && ui.get_settings_page() == index
        }));
    }
    steps.extend([
        step("leave Settings", |ui, _| {
            click(ui, 70., 64.);
            ui.get_active_page() == 0
        }),
        step("return to Settings", |ui, _| {
            click(ui, 70., 620.);
            ui.get_active_page() == 2
        }),
        step("restore scroll position", |ui, s| {
            (ui.get_settings_scroll_y() - s.scroll.get()).abs() < 1.
        }),
        step("locate General", |ui, _| section(ui, 0)),
        step("shortcut failure restores value", |ui, _| {
            click(ui, 400., 280.);
            key(ui, "z".into());
            click(ui, 870., 280.);
            ui.get_settings_error() == "Fixture shortcut error"
                && ui.get_global_hotkey() == "Alt+D"
                && ui.get_settings_feedback_kind() == FeedbackKind::Error
        }),
        step("dismiss shortcut feedback", |ui, _| {
            ui.set_settings_error(SharedString::default());
            true
        }),
        step("startup failure rolls back switch", |ui, s| {
            click(ui, 895., 344.);
            !ui.get_start_at_login()
                && s.startup_calls.get() == 1
                && ui.get_settings_feedback_kind() == FeedbackKind::Error
        }),
        step("dismiss startup feedback", |ui, _| {
            ui.set_settings_error(SharedString::default());
            true
        }),
        step("launch preference succeeds", |ui, _| {
            click(ui, 895., 420.);
            ui.get_launch_minimized() && ui.get_settings_feedback_kind() == FeedbackKind::Success
        }),
        step("locate Translation", |ui, _| section(ui, 1)),
        step("provider proxy controlled", |ui, _| {
            click(ui, 895., 370.);
            ui.get_provider_proxy_enabled()
        }),
        step("provider test receives entered value", |ui, s| {
            click(ui, 400., 310.);
            key(ui, "fixture-key".into());
            click(ui, 800., 430.);
            s.provider_tests.get() == 1 && !s.provider_key_empty.get()
        }),
        step("scroll Translation away", |ui, _| section(ui, 3)),
        step("return to Translation", |ui, _| section(ui, 1)),
        step("API key cleared when offscreen", |ui, s| {
            click(ui, 800., 430.);
            s.provider_tests.get() == 2 && s.provider_key_empty.get()
        }),
        step("validation prevents save", |ui, s| {
            click(ui, 400., 310.);
            key(ui, "fixture-key".into());
            s.reject_provider.set(true);
            click(ui, 895., 430.);
            s.provider_saves.get() == 0
                && ui.get_settings_error() == "Fixture validation error"
                && ui.get_settings_feedback_kind() == FeedbackKind::Error
        }),
        step("dismiss provider error", |ui, _| {
            ui.set_settings_error(SharedString::default());
            true
        }),
        step("valid provider save has success feedback", |ui, s| {
            s.reject_provider.set(false);
            click(ui, 895., 430.);
            s.provider_saves.get() == 1
                && s.provider_proxy_saved.get()
                && ui.get_settings_feedback_kind() == FeedbackKind::Success
        }),
        step("dismiss provider success", |ui, _| {
            ui.set_settings_error(SharedString::default());
            true
        }),
        step("submission clears key", |ui, s| {
            click(ui, 800., 430.);
            s.provider_tests.get() == 3 && s.provider_key_empty.get()
        }),
        step("locate Account", |ui, _| section(ui, 2)),
        step("inert login receives password", |ui, s| {
            click(ui, 400., 387.);
            key(ui, "fixture-password".into());
            click(ui, 885., 430.);
            s.logins.get() == 1 && !s.password_empty.get()
        }),
        step("scroll Account away", |ui, _| section(ui, 3)),
        step("return to Account", |ui, _| section(ui, 2)),
        step("password cleared when offscreen", |ui, s| {
            click(ui, 885., 430.);
            s.logins.get() == 2 && s.password_empty.get()
        }),
        step("locate Devices", |ui, _| section(ui, 4)),
        step("device identity and disabled revoke", |ui, s| {
            click(ui, 882., 220.);
            click(ui, 882., 336.);
            s.revoked.get() == 1 && s.revoked_expected_id.get()
        }),
        step("locate History settings", |ui, _| section(ui, 5)),
        step("clear history reaches host", |ui, s| {
            click(ui, 855., 220.);
            s.clear_calls.get() == 1
        }),
        step("locate Data", |ui, _| section(ui, 6)),
        step("import and export reach host", |ui, s| {
            click(ui, 740., 280.);
            click(ui, 865., 280.);
            s.imports.get() == 1 && s.exports.get() == 1
        }),
        step("locate Update", |ui, _| section(ui, 7)),
        step("shared network field values", |ui, s| {
            click(ui, 895., 486.);
            click(ui, 730., 546.);
            ui.get_update_proxy_enabled()
                && s.network_args_valid.get()
                && ui.get_settings_feedback_kind() == FeedbackKind::Success
        }),
        step("dismiss network feedback", |ui, _| {
            ui.set_settings_error(SharedString::default());
            true
        }),
        step("check updates reaches host", |ui, s| {
            click(ui, 850., 546.);
            s.updates.get() == 1
        }),
        step("return to History list", |ui, _| {
            click(ui, 70., 64.);
            ui.get_active_page() == 0
        }),
        step("History search binding", |ui, s| {
            click(ui, 400., 125.);
            key(ui, "x".into());
            ui.get_search_text() == "x" && s.history_search.get() == 1
        }),
        step("History current favorite false", |ui, s| {
            click(ui, 900., 232.);
            s.favorite_state.get() == Some(false)
        }),
        step("Favorites shares search", |ui, _| {
            click(ui, 70., 104.);
            ui.get_active_page() == 1 && ui.get_search_text() == "x"
        }),
        step("Favorites search binding", |ui, s| {
            click(ui, 400., 125.);
            key(ui, "y".into());
            ui.get_search_text().contains('y') && s.favorites_search.get() == 1
        }),
        step("Favorites current favorite true", |ui, s| {
            click(ui, 900., 232.);
            s.favorite_state.get() == Some(true)
        }),
        step("narrow single-column Settings", |ui, _| {
            ui.set_active_page(2);
            ui.set_settings_page(0);
            ui.window().set_size(LogicalSize::new(760., 540.));
            true
        }),
        step("wheel scroll", |ui, _| {
            ui.window().dispatch_event(WindowEvent::PointerScrolled {
                position: LogicalPosition::new(450., 380.),
                delta_x: 0.,
                delta_y: -10000.,
            });
            true
        }),
        step("scroll keeps navigation state", |ui, s| {
            s.scroll.set(ui.get_settings_scroll_y());
            ui.get_settings_scroll_y() < -100.
                && !ui.get_sidebar_collapsed()
                && ui.get_settings_page() == 0
        }),
        step("leave narrow Settings", |ui, _| {
            ui.set_active_page(0);
            true
        }),
        step("return to narrow Settings", |ui, _| {
            ui.set_active_page(2);
            true
        }),
        step("restore wheel scroll", |ui, s| {
            (ui.get_settings_scroll_y() - s.scroll.get()).abs() < 1.
        }),
    ]);
    steps.extend([
        step("restore width and request Translation", |ui, _| {
            ui.window().set_size(LogicalSize::new(960., 640.));
            section(ui, 1)
        }),
        step("locate General for registration feedback", |ui, _| {
            section(ui, 0)
        }),
        step(
            "partial shortcut success keeps active registration",
            |ui, s| {
                s.shortcut_partial.set(true);
                click(ui, 400., 280.);
                key(ui, "x".into());
                click(ui, 870., 280.);
                ui.get_global_hotkey().contains('x')
                    && ui.get_settings_error() == "Fixture shortcut preference error"
                    && ui.get_settings_feedback_kind() == FeedbackKind::Error
            },
        ),
        step("dismiss partial shortcut feedback", |ui, _| {
            ui.set_settings_error(SharedString::default());
            true
        }),
        step(
            "registration failure retains last effective shortcut",
            |ui, s| {
                s.shortcut_partial.set(false);
                let previous = ui.get_global_hotkey();
                click(ui, 400., 280.);
                key(ui, "y".into());
                click(ui, 870., 280.);
                ui.get_global_hotkey() == previous
                    && ui.get_settings_error() == "Fixture shortcut error"
            },
        ),
    ]);
    steps
}
fn main() -> Result<(), Box<dyn Error>> {
    let ui = MainWindow::new()?;
    ui.set_global_hotkey("Alt+D".into());
    ui.set_proxy_kind(1);
    ui.set_proxy_address("127.0.0.1:7890".into());
    let mut record = UiRecord {
        key: "fixture-only".into(),
        source: "invariant".into(),
        translation: "Synthetic translation".into(),
        count: 1,
        favorite: false,
        metadata: SharedString::default(),
    };
    ui.set_history_records(slint::ModelRc::new(slint::VecModel::from(vec![
        record.clone(),
    ])));
    record.favorite = true;
    ui.set_favorite_records(slint::ModelRc::new(slint::VecModel::from(vec![record])));
    let mut device = DeviceRecord {
        id: "fixture-device".into(),
        name: "Synthetic device".into(),
        platform: "Fixture platform".into(),
        last_seen: "Fixture".into(),
        current: true,
        revoked: false,
    };
    let mut devices = vec![device.clone()];
    device.id = "revoked-device".into();
    device.current = false;
    device.revoked = true;
    devices.push(device);
    ui.set_devices(slint::ModelRc::new(slint::VecModel::from(devices)));
    let signals = Rc::new(Signals::default());
    connect(&ui, &signals);
    ui.show()?;
    ui.window().set_size(LogicalSize::new(960., 640.));
    let failed = Rc::new(Cell::new(false));
    let failure = Rc::clone(&failed);
    let weak = ui.as_weak();
    let index = Cell::new(0);
    let steps = steps();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(250),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let (name, action) = &steps[index.get()];
            let passed = action(&ui, &signals);
            println!(
                "{} {}: {name}",
                if passed { "PASS" } else { "FAIL" },
                index.get()
            );
            failure.set(failure.get() || !passed);
            index.set(index.get() + 1);
            if index.get() == steps.len() {
                let _ = slint::quit_event_loop();
            }
        },
    );
    slint::run_event_loop()?;
    if failed.get() {
        return Err("UI interaction check failed".into());
    }
    Ok(())
}
