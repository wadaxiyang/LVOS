#![cfg(any(target_os = "macos", target_os = "windows"))]

use lvos::{PopupLifecycleState, UiProcessCoordinator};
use slint::ComponentHandle;
use std::{cell::Cell, rc::Rc};

#[test]
fn coordinator_starts_cold_and_recreates_the_management_host()
-> Result<(), Box<dyn std::error::Error>> {
    let ui = UiProcessCoordinator::new()?;

    assert!(!ui.has_main_host());
    assert!(!ui.has_popup_host());
    assert!(!ui.has_permission_host());
    assert!(!ui.has_live_ui());
    assert_eq!(ui.popup_lifecycle_state(), PopupLifecycleState::Cold);

    let creations = Rc::new(Cell::new(0_u32));
    let hook_creations = Rc::clone(&creations);
    ui.on_main_created(move |_, _| {
        hook_creations.set(hook_creations.get().saturating_add(1));
    });
    let first = ui.main_window().as_weak();
    assert!(ui.has_main_host());
    assert!(ui.has_live_ui());
    assert_eq!(creations.get(), 1);

    ui.hide_main_window()?;
    assert!(!ui.has_main_host());
    assert!(!ui.has_live_ui());
    assert!(first.upgrade().is_none());

    let second = ui.main_window().as_weak();
    assert!(ui.has_main_host());
    assert!(second.upgrade().is_some());
    assert_eq!(creations.get(), 2);

    let permission = ui.permission_window().as_weak();
    assert!(ui.has_permission_host());
    ui.hide_permission_window()?;
    assert!(!ui.has_permission_host());
    assert!(permission.upgrade().is_none());
    let popup = ui.popup().as_weak();

    assert!(ui.has_popup_host());
    assert_eq!(ui.popup_lifecycle_state(), PopupLifecycleState::Warm);

    ui.set_popup_idle_timeout_secs(0)?;

    assert!(!ui.has_popup_host());
    assert_eq!(ui.popup_lifecycle_state(), PopupLifecycleState::Cold);
    assert!(popup.upgrade().is_none());
    assert!(ui.has_live_ui());
    ui.hide_main_window()?;
    assert!(!ui.has_live_ui());
    Ok(())
}
