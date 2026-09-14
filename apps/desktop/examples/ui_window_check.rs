//! Windows native lifecycle checks on synthetic windows; no services or user data.
#[cfg(not(target_os = "windows"))]
fn main() {
    println!("NOT_RUN: this fixture requires Windows");
}

#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::run()
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
                GetCursorPos, GetForegroundWindow, GetWindowRect, SetCursorPos, SetForegroundWindow,
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
        let failed = Rc::new(Cell::new(false));
        let failure = Rc::clone(&failed);
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(400),
            move || {
                let foreground_is_main =
                    || hwnd(ui.main_window().window()) == Some(unsafe { GetForegroundWindow() });
                if [2, 5, 6, 16].contains(&index.get()) {
                    println!(
                        "state step={} visible={} anchor={} focus={:?} favorite={} refresh={}",
                        index.get(),
                        ui.popup().window().is_visible(),
                        foreground_is_main(),
                        ui.popup_focus(),
                        favorite.get(),
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
                            && foreground_is_main()
                            && ui.popup_focus() == PopupFocusState::VisibleNoActivate,
                    ),
                    3 => ("native favorite click", click_action(&ui, false)),
                    4 => {
                        let once =
                            favorite.get() == 1 && ui.popup_focus() == PopupFocusState::Interactive;
                        let clicked = click_action(&ui, true);
                        (
                            "favorite fires once and starts interaction",
                            once && clicked,
                        )
                    }
                    5 => {
                        let once = refresh.get() == 1;
                        let once = once && escape();
                        ("refresh fires once; Escape", once)
                    }
                    6 => {
                        let hidden = !ui.popup().window().is_visible()
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
                        let no_activate = foreground_is_main() && ui.popup().window().is_visible();
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
                        ("native outside click", ready && outside_click(&ui))
                    }
                    11 => (
                        "outside click removes monitor and clears state",
                        !ui.popup().window().is_visible()
                            && ui.popup_focus() == PopupFocusState::Hidden,
                    ),
                    12 => {
                        let queued = ui.show_lookup_card(&ready).is_ok();
                        let hidden = ui.hide_lookup_card().is_ok();
                        ("cancel before pending show runs", queued && hidden)
                    }
                    13 => (
                        "cancelled creation cannot reopen popup",
                        !ui.popup().window().is_visible(),
                    ),
                    14 => {
                        let _ = ui.show_lookup_card(&ready);
                        let _ = ui.show_lookup_card(&ready);
                        ("rapid replacement", true)
                    }
                    15 => (
                        "rapid replacement retains one active dismissal path",
                        ui.popup().window().is_visible() && outside_click(&ui),
                    ),
                    16 => (
                        "repeated outside close",
                        !ui.popup().window().is_visible()
                            && favorite.get() == 1
                            && refresh.get() == 1,
                    ),
                    17..=256 => {
                        if index.get() % 2 == 1 {
                            let hidden = !ui.popup().window().is_visible();
                            ("cycle show", hidden && ui.show_lookup_card(&ready).is_ok())
                        } else {
                            let visible = ui.popup().window().is_visible() && foreground_is_main();
                            (
                                "cycle no-activate/hide",
                                visible && ui.hide_lookup_card().is_ok(),
                            )
                        }
                    }
                    _ => (
                        "120 production popup cycles complete",
                        !ui.popup().window().is_visible(),
                    ),
                };
                if index.get() <= 16 || index.get() == 257 || !ok {
                    println!(
                        "{} {}: {name}",
                        if ok { "PASS" } else { "FAIL" },
                        index.get()
                    );
                }
                failure.set(failure.get() || !ok);
                if index.get() == 257 {
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
