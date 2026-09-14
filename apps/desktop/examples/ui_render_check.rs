//! Render synthetic UI scenes; output is a portable PPM image.
#![allow(clippy::unwrap_used, clippy::panic)] // Fail loudly on renderer/file errors in this diagnostic.
use slint::ComponentHandle;
use std::{error::Error, io::Write, time::Duration};

fn render<T: ComponentHandle + 'static>(
    component: &T,
    output: String,
) -> Result<(), Box<dyn Error>> {
    let ready = std::rc::Rc::new(std::cell::Cell::new(false));
    let armed = ready.clone();
    let weak = component.as_weak();
    component.window().set_rendering_notifier(move |state, _| {
        if !matches!(state, slint::RenderingState::AfterRendering) || !armed.replace(false) {
            return;
        }
        let component = weak.upgrade().unwrap();
        let snapshot = component.window().take_snapshot().unwrap();
        assert!(
            snapshot
                .as_bytes()
                .chunks_exact(4)
                .any(|p| p[0] > 0 || p[1] > 0 || p[2] > 0),
            "blank renderer snapshot"
        );
        let mut file = std::fs::File::create(&output).unwrap();
        write!(
            file,
            "P6\n{} {}\n255\n",
            snapshot.width(),
            snapshot.height()
        )
        .unwrap();
        for pixel in snapshot.as_bytes().chunks_exact(4) {
            file.write_all(&pixel[..3]).unwrap();
        }
        println!(
            "snapshot {}x{} -> {}",
            snapshot.width(),
            snapshot.height(),
            output
        );
        slint::quit_event_loop().unwrap();
    })?;
    component.show()?;
    let weak = component.as_weak();
    slint::Timer::single_shot(Duration::from_millis(700), move || {
        ready.set(true);
        weak.upgrade().unwrap().window().request_redraw();
    });
    slint::run_event_loop()?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    let scenario = args.get(1).map_or("history", String::as_str);
    let output = args.get(2).ok_or("output path required")?.clone();
    if scenario == "permission" {
        return render(&lvos::PermissionWindow::new()?, output);
    }
    if matches!(scenario, "loading" | "ready" | "error" | "long") {
        let popup = lvos::QuickLookupPopup::new()?;
        popup.set_source_text("invariant".into());
        popup.set_translated_text(if scenario == "long" {
            "这是一段仅用于验证布局与资源的虚构长译文。"
                .repeat(35)
                .into()
        } else {
            "不变的；恒定的".into()
        });
        popup.set_loading(scenario == "loading");
        popup.set_error_visible(scenario == "error");
        popup.set_error_title("Fixture error".into());
        popup.set_error_detail("No external service was contacted.".into());
        popup.set_effective_count(8);
        popup.set_favorite(true);
        return render(&popup, output);
    }
    let main = lvos::MainWindow::new()?;
    main.set_global_hotkey("Alt+D".into());
    if args.get(3).map(String::as_str) == Some("collapsed") {
        main.set_sidebar_collapsed(true);
    }
    if args.get(3).map(String::as_str) == Some("narrow") {
        main.window().set_size(slint::LogicalSize::new(760., 540.));
    }
    let fixture = lvos::UiRecord {
        key: "fixture-only".into(),
        source: "invariant".into(),
        translation: "不变的；恒定的".into(),
        count: 8,
        favorite: true,
        metadata: "Synthetic baseline".into(),
    };
    main.set_history_records(slint::ModelRc::new(slint::VecModel::from(vec![
        fixture.clone(),
    ])));
    main.set_favorite_records(slint::ModelRc::new(slint::VecModel::from(vec![fixture])));
    main.set_devices(slint::ModelRc::new(slint::VecModel::from(vec![
        lvos::DeviceRecord {
            id: "fixture-device".into(),
            name: "Synthetic device".into(),
            platform: "Windows 11".into(),
            last_seen: "Fixture".into(),
            current: true,
            revoked: false,
        },
    ])));
    match scenario {
        "modal" => {
            main.set_active_page(2);
            main.set_settings_page(6);
            main.set_confirmation_title("Import LVOS data?".into());
            main.set_confirmation_message("History: 12 add, 3 update. Favorites: 8 add, 1 reactivate. QueryStats archive: 21 records. Nothing changes until you choose Import.".into());
            main.set_confirmation_primary("Import".into());
            main.set_confirmation_shown(true);
        }
        "history-long" => {
            main.set_history_records(slint::ModelRc::new(slint::VecModel::from(
                (0..200)
                    .map(|i| lvos::UiRecord {
                        key: format!("synthetic-{i}").into(),
                        source: "A longer synthetic phrase for checking a narrow list layout"
                            .into(),
                        translation: "这是一段用于验证列表换行和动态行高的虚构译文。"
                            .repeat(8)
                            .into(),
                        count: i + 1,
                        favorite: false,
                        metadata: "Synthetic layout fixture".into(),
                    })
                    .collect::<Vec<_>>(),
            )));
        }
        "feedback-error" | "feedback-success" | "feedback-long" => {
            main.set_active_page(2);
            main.set_settings_feedback_kind(if scenario == "feedback-error" {
                lvos::FeedbackKind::Error
            } else {
                lvos::FeedbackKind::Success
            });
            main.set_settings_error(if scenario == "feedback-error" { "Synthetic failure: the requested operation could not finish. This longer message checks wrapping while the settings content remains scrollable." } else { "Synthetic success: preferences saved." }.into());
            if scenario == "feedback-long" {
                main.set_settings_feedback_kind(lvos::FeedbackKind::Error);
                main.set_settings_error("Synthetic long diagnostic detail. No real service or personal data is involved. ".repeat(30).into());
            }
        }
        "favorites" => main.set_active_page(1),
        "history-empty" => main.set_history_records(slint::ModelRc::default()),
        "favorites-empty" => {
            main.set_active_page(1);
            main.set_favorite_records(slint::ModelRc::default());
        }
        _ => {
            let pages = [
                "general",
                "translation",
                "account",
                "sync",
                "devices",
                "settings-history",
                "data",
                "update",
            ];
            if let Some(page) = pages.iter().position(|s| *s == scenario) {
                main.set_active_page(2);
                main.set_settings_page(i32::try_from(page)?);
            }
        }
    }
    render(&main, output)
}
