//! Render synthetic UI scenes; output is a portable PPM image.
#![allow(clippy::unwrap_used, clippy::panic)] // Fail loudly on renderer/file errors in this diagnostic.
use slint::ComponentHandle;
use std::{error::Error, io::Write, time::Duration};

fn render<T: ComponentHandle + 'static>(
    component: &T,
    output: String,
) -> Result<(), Box<dyn Error>> {
    component.show()?;
    let weak = component.as_weak();
    slint::Timer::single_shot(Duration::from_millis(700), move || {
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
    });
    slint::run_event_loop_until_quit()?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    let scenario = args.get(1).map_or("history", String::as_str);
    let output = args.get(2).ok_or("output path required")?.clone();
    let dark = args.iter().any(|arg| arg == "dark");
    let reduced = args.iter().any(|arg| arg == "reduced");
    if scenario == "permission" {
        let permission = lvos::PermissionWindow::new()?;
        permission.set_dark_theme(dark);
        permission.set_reduce_motion(reduced);
        return render(&permission, output);
    }
    if matches!(scenario, "loading" | "ready" | "error" | "long") {
        let popup = lvos::QuickLookupPopup::new()?;
        // Production sizing clamps every lookup state to at least 240px.
        popup.set_popup_height(240.);
        popup.set_dark_theme(dark);
        popup.set_reduce_motion(reduced);
        popup.set_source_text(if scenario == "long" {
            "A deliberately long English source sentence that must stay on one line and end with an ellipsis instead of wrapping across the popup.".into()
        } else {
            "invariant".into()
        });
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
    main.set_dark_theme(dark);
    main.set_reduce_motion(reduced);
    main.set_global_hotkey("Alt+D".into());
    main.set_tokenhub_configured(true);
    if args.iter().any(|arg| arg == "collapsed") {
        main.set_sidebar_collapsed(true);
    }
    if args.iter().any(|arg| arg == "narrow") {
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
        "history-detail" => {
            main.set_history_records(slint::ModelRc::new(slint::VecModel::from(vec![
                lvos::UiRecord {
                    key: "detail-fixture".into(),
                    source: "A complete multi-sentence English source remains available in the detail page even though the History list shows an ellipsis.".repeat(4).into(),
                    translation: "详细页面完整保留译文的段落结构。\n\n这是第二段，用于检查长内容是否可以滚动阅读。".repeat(8).into(),
                    count: 9,
                    favorite: false,
                    metadata: "Synthetic detail fixture".into(),
                },
            ])));
            main.set_history_detail_index(0);
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
