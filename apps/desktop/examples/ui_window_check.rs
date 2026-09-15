//! Windows native lifecycle checks on synthetic windows; no services or user data.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    println!("NOT_RUN: this fixture requires Windows or macOS");
}

#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_diagnostics();
    native::run()
}

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_diagnostics();
    macos::run()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn init_diagnostics() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn require_destroy_on_hide() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("SLINT_DESTROY_WINDOW_ON_HIDE").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return Err("run with SLINT_DESTROY_WINDOW_ON_HIDE=1".into());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)] // Native test input is limited to the fixture's own windows.
mod native {
    use lvos::{LookupCardState, PopupFocusState, PopupLifecycleState, UiController};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::{ComponentHandle, winit_030::WinitWindowAccessor};
    use std::{cell::Cell, error::Error, rc::Rc, str::FromStr, time::Duration};
    use windows::Win32::{
        Foundation::{HWND, POINT, RECT},
        UI::{
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
                KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput,
                VK_ESCAPE,
            },
            WindowsAndMessaging::{
                GetCursorPos, GetForegroundWindow, GetWindowRect, IsWindow, SetCursorPos,
                SetForegroundWindow,
            },
        },
    };

    fn hwnd(window: &slint::Window) -> Option<HWND> {
        let handle = window.window_handle();
        let raw = handle.window_handle().ok()?.as_raw();
        if let RawWindowHandle::Win32(value) = raw {
            Some(HWND(value.hwnd.get() as *mut _))
        } else {
            None
        }
    }
    fn bounds(window: &slint::Window) -> Option<RECT> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(hwnd(window)?, &raw mut rect) }.ok()?;
        Some(rect)
    }
    fn native_window_destroyed(hwnd: Option<isize>) -> bool {
        hwnd.is_some_and(|hwnd| !unsafe { IsWindow(Some(HWND(hwnd as *mut _))) }.as_bool())
    }
    fn click(x: i32, y: i32) -> bool {
        if unsafe { SetCursorPos(x, y) }.is_err() {
            return false;
        }
        let events = [MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP].map(|flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        });
        let Ok(size) = i32::try_from(std::mem::size_of::<INPUT>()) else {
            return false;
        };
        slint::Timer::single_shot(Duration::from_millis(50), move || {
            let _ = unsafe { SendInput(&events, size) };
        });
        true
    }
    fn escape() -> bool {
        let events = [KEYBD_EVENT_FLAGS::default(), KEYEVENTF_KEYUP].map(|flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_ESCAPE,
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        });
        let Ok(size) = i32::try_from(std::mem::size_of::<INPUT>()) else {
            return false;
        };
        (unsafe { SendInput(&events, size) }) == 2
    }
    #[allow(clippy::cast_possible_truncation)]
    fn click_action(ui: &UiController, footer: bool) -> bool {
        let Some(rect) = bounds(ui.popup().window()) else {
            return false;
        };
        let scale = ui.popup().window().scale_factor();
        click(
            rect.right - (38. * scale) as i32,
            if footer {
                rect.bottom - (32. * scale) as i32
            } else {
                rect.top + (32. * scale) as i32
            },
        )
    }
    #[allow(clippy::cast_possible_truncation)]
    fn click_copy(ui: &UiController) -> bool {
        let Some(rect) = bounds(ui.popup().window()) else {
            return false;
        };
        let scale = ui.popup().window().scale_factor();
        click(
            rect.right - (38. * scale) as i32,
            rect.bottom - (72. * scale) as i32,
        )
    }
    fn outside_click(ui: &UiController) -> bool {
        let Some(main) = bounds(ui.main_window().window()) else {
            return false;
        };
        let Some(popup) = bounds(ui.popup().window()) else {
            return false;
        };
        for (x, y) in [
            (main.left + 25, main.top + 60),
            (main.right - 25, main.top + 60),
            (main.left + 25, main.bottom - 25),
        ] {
            if x < popup.left || x >= popup.right || y < popup.top || y >= popup.bottom {
                return click(x, y);
            }
        }
        false
    }
    fn focus_main(ui: &UiController) {
        ui.main_window()
            .window()
            .with_winit_window(slint::winit_030::winit::window::Window::focus_window);
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn run() -> Result<(), Box<dyn Error>> {
        super::require_destroy_on_hide()?;
        let mut original_cursor = POINT::default();
        unsafe { GetCursorPos(&raw mut original_cursor) }?;
        let original_foreground = unsafe { GetForegroundWindow() };
        let ui = Rc::new(UiController::new()?);
        let favorite = Rc::new(Cell::new(0));
        let count = Rc::clone(&favorite);
        ui.popup()
            .on_favorite_toggled(move || count.set(count.get() + 1));
        let refresh = Rc::new(Cell::new(0));
        let count = Rc::clone(&refresh);
        ui.popup()
            .on_refresh_requested(move || count.set(count.get() + 1));
        let copy = Rc::new(Cell::new(0));
        let count = Rc::clone(&copy);
        ui.popup().on_copy_requested(move |text| {
            if text.as_str() == "Synthetic translation" {
                count.set(count.get() + 1);
            }
        });
        let ready = LookupCardState::Ready {
            generation: 2,
            content_key: lvos_core::ContentKey::from_str(
                "ce60ddcf96e4c4c3f94a305956a98de6afdebf59e8c6bd10b285b73b06949f08",
            )?,
            source: "invariant".into(),
            translation: "Synthetic translation".into(),
            favorite: false,
            effective_query_count: 2,
        };
        ui.show_main_window()?;
        let index = Cell::new(0);
        let last_popup_hwnd = Cell::new(None);
        let failed = Rc::new(Cell::new(false));
        let failure = Rc::clone(&failed);
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            // Native window and Skia surface recreation is asynchronous. Leave a full event-loop
            // turn plus GPU initialization time between each observable transition.
            Duration::from_secs(1),
            move || {
                let foreground_is_main =
                    || hwnd(ui.main_window().window()) == Some(unsafe { GetForegroundWindow() });
                if [2, 5, 6, 16].contains(&index.get()) {
                    println!(
                        "state step={} visible={} anchor={} focus={:?} favorite={} copy={} refresh={}",
                        index.get(),
                        ui.popup().window().is_visible(),
                        foreground_is_main(),
                        ui.popup_focus(),
                        favorite.get(),
                        copy.get(),
                        refresh.get()
                    );
                }
                let (name, ok) = match index.get() {
                    0 => {
                        focus_main(&ui);
                        ("focus fixture anchor", true)
                    }
                    1 => {
                        let focused = foreground_is_main();
                        let queued = ui.show_lookup_card(&ready).is_ok();
                        (
                            "controller queues hidden native creation",
                            focused && queued,
                        )
                    }
                    2 => (
                        "controller first show never activates popup",
                        ui.popup().window().is_visible()
                            && ui.popup().window().has_winit_window()
                            && foreground_is_main()
                            && ui.popup_focus() == PopupFocusState::VisibleNoActivate,
                    ),
                    3 => {
                        let clicked = click_action(&ui, false);
                        let copy_ui = Rc::clone(&ui);
                        slint::Timer::single_shot(Duration::from_millis(150), move || {
                            let _ = click_copy(&copy_ui);
                        });
                        ("native favorite and copy clicks", clicked)
                    }
                    4 => {
                        let once = favorite.get() == 1
                            && copy.get() == 1
                            && ui.popup_focus() == PopupFocusState::Interactive;
                        let clicked = click_action(&ui, true);
                        (
                            "favorite/copy fire once and start interaction",
                            once && clicked,
                        )
                    }
                    5 => {
                        let once = refresh.get() == 1;
                        last_popup_hwnd.set(hwnd(ui.popup().window()).map(|hwnd| hwnd.0 as isize));
                        let once = once && escape();
                        ("refresh fires once; Escape", once)
                    }
                    6 => {
                        let hidden = !ui.popup().window().is_visible()
                            && !ui.popup().window().has_winit_window()
                            && native_window_destroyed(last_popup_hwnd.get())
                            && ui.popup_focus() == PopupFocusState::Hidden
                            && ui.has_popup_host()
                            && ui.popup_lifecycle_state() == PopupLifecycleState::Warm;
                        focus_main(&ui);
                        ("Escape clears lifecycle", hidden)
                    }
                    7 => {
                        let queued = ui
                            .show_lookup_card(&LookupCardState::Loading {
                                generation: 2,
                                source: "synthetic".into(),
                            })
                            .is_ok();
                        ("production Loading path", queued)
                    }
                    8 => {
                        let no_activate = foreground_is_main()
                            && ui.popup().window().is_visible()
                            && ui.popup().window().has_winit_window();
                        let queued = ui.show_lookup_card(&ready).is_ok();
                        (
                            "Loading no-activate and Ready replacement",
                            no_activate && queued,
                        )
                    }
                    9 => {
                        let no_activate = foreground_is_main();
                        let queued = ui.show_captured_provider_error("synthetic source").is_ok();
                        ("captured-provider error path", no_activate && queued)
                    }
                    10 => {
                        let ready = foreground_is_main() && ui.popup().get_error_visible();
                        last_popup_hwnd.set(hwnd(ui.popup().window()).map(|hwnd| hwnd.0 as isize));
                        ("native outside click", ready && outside_click(&ui))
                    }
                    11 => (
                        "outside click removes monitor and clears state",
                        !ui.popup().window().is_visible()
                            && !ui.popup().window().has_winit_window()
                            && native_window_destroyed(last_popup_hwnd.get())
                            && ui.popup_focus() == PopupFocusState::Hidden,
                    ),
                    12 => {
                        let queued = ui.show_lookup_card(&ready).is_ok();
                        let hidden = ui.hide_lookup_card().is_ok();
                        ("cancel before pending show runs", queued && hidden)
                    }
                    13 => (
                        "cancelled creation cannot reopen popup",
                        !ui.popup().window().is_visible()
                            && !ui.popup().window().has_winit_window(),
                    ),
                    14 => {
                        let _ = ui.show_lookup_card(&ready);
                        let _ = ui.show_lookup_card(&ready);
                        ("rapid replacement", true)
                    }
                    15 => (
                        "rapid replacement retains one active dismissal path",
                        ui.popup().window().is_visible()
                            && {
                                last_popup_hwnd.set(
                                    hwnd(ui.popup().window()).map(|hwnd| hwnd.0 as isize),
                                );
                                outside_click(&ui)
                            },
                    ),
                    16 => (
                        "repeated outside close",
                        !ui.popup().window().is_visible()
                            && !ui.popup().window().has_winit_window()
                            && native_window_destroyed(last_popup_hwnd.get())
                            && favorite.get() == 1
                            && copy.get() == 1
                            && refresh.get() == 1,
                    ),
                    17..=32 => {
                        if index.get() % 2 == 1 {
                            let hidden = !ui.popup().window().is_visible()
                                && !ui.popup().window().has_winit_window()
                                && native_window_destroyed(last_popup_hwnd.get());
                            ("cycle show", hidden && ui.show_lookup_card(&ready).is_ok())
                        } else {
                            let visible = ui.popup().window().is_visible()
                                && ui.popup().window().has_winit_window()
                                && foreground_is_main();
                            last_popup_hwnd
                                .set(hwnd(ui.popup().window()).map(|hwnd| hwnd.0 as isize));
                            (
                                "cycle no-activate/hide",
                                visible && ui.hide_lookup_card().is_ok(),
                            )
                        }
                    }
                    _ => (
                        "8 production popup recreation cycles complete",
                        !ui.popup().window().is_visible()
                            && !ui.popup().window().has_winit_window(),
                    ),
                };
                if index.get() <= 16 || index.get() == 33 || !ok {
                    println!(
                        "{} {}: {name}",
                        if ok { "PASS" } else { "FAIL" },
                        index.get()
                    );
                }
                if !ok {
                    println!(
                        "  visible={} native={} anchor={} focus={:?} previous-native-destroyed={}",
                        ui.popup().window().is_visible(),
                        ui.popup().window().has_winit_window(),
                        foreground_is_main(),
                        ui.popup_focus(),
                        native_window_destroyed(last_popup_hwnd.get())
                    );
                }
                failure.set(failure.get() || !ok);
                if index.get() == 33 {
                    let _ = slint::quit_event_loop();
                }
                index.set(index.get() + 1);
            },
        );
        slint::run_event_loop_until_quit()?;
        let _ = unsafe { SetCursorPos(original_cursor.x, original_cursor.y) };
        let _ = unsafe { SetForegroundWindow(original_foreground) };
        if failed.get() {
            return Err("native window lifecycle check failed".into());
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use lvos::{LookupCardState, PopupFocusState, PopupLifecycleState, UiController};
    use slint::{ComponentHandle, winit_030::WinitWindowAccessor};
    use std::{cell::Cell, error::Error, rc::Rc, str::FromStr, time::Duration};

    pub(super) fn run() -> Result<(), Box<dyn Error>> {
        super::require_destroy_on_hide()?;
        let ui = Rc::new(UiController::new()?);
        let ready = LookupCardState::Ready {
            generation: 1,
            content_key: lvos_core::ContentKey::from_str(
                "ce60ddcf96e4c4c3f94a305956a98de6afdebf59e8c6bd10b285b73b06949f08",
            )?,
            source: "invariant".into(),
            translation: "Synthetic translation".into(),
            favorite: false,
            effective_query_count: 1,
        };
        let step = Cell::new(0_u8);
        let failed = Rc::new(Cell::new(false));
        let failure = Rc::clone(&failed);
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(500),
            move || {
                let shown = step.get() % 2 == 1;
                let (name, ok) = if shown {
                    let visible = ui.popup().window().is_visible()
                        && ui.popup().window().has_winit_window()
                        && ui.popup_focus() == PopupFocusState::VisibleNoActivate;
                    (
                        "recreated popup is visible without activation",
                        visible && ui.hide_lookup_card().is_ok(),
                    )
                } else {
                    let hidden = step.get() == 0
                        || (!ui.popup().window().is_visible()
                            && !ui.popup().window().has_winit_window()
                            && ui.popup_lifecycle_state() == PopupLifecycleState::Warm);
                    (
                        "hidden popup releases and recreates its native window",
                        hidden && ui.show_lookup_card(&ready).is_ok(),
                    )
                };
                println!(
                    "{} {}: {name}",
                    if ok { "PASS" } else { "FAIL" },
                    step.get()
                );
                failure.set(failure.get() || !ok);
                if step.get() == 6 {
                    let _ = ui.hide_lookup_card();
                    let _ = slint::quit_event_loop();
                }
                step.set(step.get() + 1);
            },
        );
        slint::run_event_loop_until_quit()?;
        if failed.get() {
            return Err("macOS native window recreation check failed".into());
        }
        Ok(())
    }
}
