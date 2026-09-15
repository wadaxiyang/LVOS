//! Exercise the migrated read-only `ActionTextArea` with synthetic data only.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used,
    reason = "interactive diagnostic fails loudly on fixture or native-input errors"
)]
#![cfg_attr(
    not(target_os = "windows"),
    allow(clippy::unnecessary_wraps, unused_imports)
)]

#[cfg(target_os = "windows")]
use lvos::QuickLookupPopup;
#[cfg(target_os = "windows")]
use slint::{
    ComponentHandle, LogicalPosition, SharedString,
    platform::{Key, PointerEventButton, WindowEvent},
};
#[cfg(target_os = "windows")]
use std::{
    cell::{Cell, RefCell},
    error::Error,
    io::Write,
    rc::Rc,
    time::Duration,
};

#[cfg(target_os = "windows")]
fn key(window: &slint::Window, text: SharedString) {
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

#[cfg(target_os = "windows")]
fn chord(window: &slint::Window, modifier: SharedString, text: SharedString) {
    window.dispatch_event(WindowEvent::KeyPressed {
        text: modifier.clone(),
    });
    key(window, text);
    window.dispatch_event(WindowEvent::KeyReleased { text: modifier });
}

#[cfg(target_os = "windows")]
fn click(window: &slint::Window, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    let button = PointerEventButton::Left;
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button });
    window.dispatch_event(WindowEvent::PointerReleased { position, button });
}

#[cfg(target_os = "windows")]
fn right_click(window: &slint::Window, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    let button = PointerEventButton::Right;
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button });
    window.dispatch_event(WindowEvent::PointerReleased { position, button });
}

#[cfg(target_os = "windows")]
fn clipboard_is(expected: &str) -> bool {
    clipboard_win::get_clipboard_string().is_ok_and(|value| value == expected)
}

#[cfg(target_os = "windows")]
fn select_native_menu(down_count: usize) -> std::thread::JoinHandle<bool> {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let keys = "{DOWN}".repeat(down_count) + "{ENTER}";
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('{keys}')"
        );
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .status()
            .is_ok_and(|status| status.success())
    })
}

#[cfg(target_os = "windows")]
fn snapshot(window: &slint::Window, name: &str) -> Vec<u8> {
    let pixels = window.take_snapshot().expect("renderer snapshot");
    let bytes = pixels.as_bytes().to_vec();
    if let Some(directory) = std::env::var_os("LVOS_QUICK_LOOKUP_SNAPSHOT_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).expect("create snapshot directory");
        let mut output = std::fs::File::create(directory.join(format!("{name}.ppm")))
            .expect("create snapshot file");
        write!(output, "P6\n{} {}\n255\n", pixels.width(), pixels.height())
            .expect("write snapshot header");
        for pixel in bytes.chunks_exact(4) {
            output
                .write_all(&pixel[..3])
                .expect("write snapshot pixels");
        }
    }
    bytes
}

#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn Error>> {
    let popup = QuickLookupPopup::new()?;
    let translation = "Synthetic translated result";
    popup.set_dark_theme(std::env::var_os("LVOS_QUICK_LOOKUP_DARK").is_some());
    popup.set_source_text("Synthetic source".into());
    popup.set_translated_text(translation.into());
    popup.set_effective_count(3);
    popup.set_loading(false);
    popup.set_error_visible(false);
    popup.set_error_title("Synthetic error".into());
    popup.set_error_detail("No external service was contacted.".into());
    let copies = Rc::new(Cell::new(0_u32));
    let copied_value = Rc::new(RefCell::new(String::new()));
    let refreshes = Rc::new(Cell::new(0_u32));
    let interactions = Rc::new(Cell::new(0_u32));
    {
        let copies = Rc::clone(&copies);
        let copied_value = Rc::clone(&copied_value);
        popup.on_copy_requested(move |value| {
            copies.set(copies.get() + 1);
            *copied_value.borrow_mut() = value.to_string();
        });
    }
    {
        let refreshes = Rc::clone(&refreshes);
        popup.on_refresh_requested(move || refreshes.set(refreshes.get() + 1));
    }
    {
        let interactions = Rc::clone(&interactions);
        popup.on_interaction_started(move || interactions.set(interactions.get() + 1));
    }
    popup.show()?;
    let original_clipboard = clipboard_win::get_clipboard_string().ok();
    let ready = Rc::new(RefCell::new(Vec::new()));
    let loading = Rc::new(RefCell::new(Vec::new()));
    let failed = Rc::new(Cell::new(false));
    let failure = Rc::clone(&failed);
    let weak = popup.as_weak();
    let step = Cell::new(0_u32);
    let context_menu_ran = Rc::new(Cell::new(false));
    let context_menu_state = Rc::clone(&context_menu_ran);
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(350),
        move || {
            let popup = weak.upgrade().expect("popup remains alive");
            let window = popup.window();
            let check = |name: &str, ok: bool| {
                println!("{}: {name}", if ok { "PASS" } else { "FAIL" });
                failure.set(failure.get() || !ok);
            };
            match step.get() {
                0 => {
                    *ready.borrow_mut() = snapshot(window, "ready");
                    click(window, 40., 90.);
                }
                1 => {
                    chord(window, Key::Control.into(), "a".into());
                    chord(window, Key::Control.into(), "c".into());
                    check(
                        "Ctrl+A and Ctrl+C copy the translated result",
                        clipboard_is(translation),
                    );
                    key(window, "X".into());
                    key(window, Key::Backspace.into());
                    check(
                        "read-only result rejects keyboard edits",
                        popup.get_translated_text() == translation,
                    );
                    check(
                        "removed edited workaround emits no synthetic interaction",
                        interactions.get() == 0,
                    );
                    key(window, Key::Tab.into());
                    key(window, Key::Return.into());
                }
                2 => {
                    check(
                        "Footer Copy preserves the product callback",
                        copies.get() == 1 && copied_value.borrow().as_str() == translation,
                    );
                    key(window, Key::Tab.into());
                    key(window, Key::Return.into());
                }
                3 => {
                    check(
                        "Footer Refresh preserves the product callback",
                        refreshes.get() == 1 && interactions.get() == 2,
                    );
                    click(window, 40., 90.);
                    chord(window, Key::Control.into(), "a".into());
                    clipboard_win::set_clipboard_string("context-menu-none")
                        .expect("set synthetic clipboard");
                    let input = select_native_menu(2);
                    right_click(window, 40., 90.);
                    let owned = input.join().unwrap_or(false);
                    context_menu_state.set(owned);
                    println!(
                        "OS_CONTEXT_MENU={}",
                        if context_menu_state.get() {
                            "OWN_PROCESS"
                        } else {
                            "NOT_RUN_FOREGROUND"
                        }
                    );
                }
                4 => {
                    check(
                        "own-process native context menu input ran",
                        context_menu_ran.get(),
                    );
                    check(
                        "read-only native context menu Copy works",
                        context_menu_ran.get() && clipboard_is(translation),
                    );
                    popup.set_loading(true);
                }
                5 => {
                    *loading.borrow_mut() = snapshot(window, "loading");
                    check(
                        "loading state replaces the result surface",
                        popup.get_loading() && *loading.borrow() != *ready.borrow(),
                    );
                    popup.set_loading(false);
                    popup.set_error_visible(true);
                }
                6 => {
                    let error = snapshot(window, "error");
                    check(
                        "error state keeps a distinct read-only result surface",
                        popup.get_error_visible()
                            && error != *ready.borrow()
                            && error != *loading.borrow(),
                    );
                    if let Some(value) = &original_clipboard {
                        let _ = clipboard_win::set_clipboard_string(value);
                    }
                    println!("RESULT={}", if failure.get() { "FAIL" } else { "PASS" });
                    let _ = slint::quit_event_loop();
                }
                _ => unreachable!(),
            }
            step.set(step.get() + 1);
        },
    );
    slint::run_event_loop_until_quit()?;
    if failed.get() {
        return Err("Quick Lookup ActionTextArea verification failed".into());
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("NOT_RUN: ui_quick_lookup_check requires Windows native clipboard/menu input");
}
