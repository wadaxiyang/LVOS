use std::{error::Error, str::FromStr};

use lvos::{DeviceRecord, LookupCardState, UiController, ui_record};
use lvos_core::ContentKey;
use lvos_translation::LookupCardErrorKind;

fn main() -> Result<(), Box<dyn Error>> {
    let ui = UiController::new()?;
    let invariant =
        ContentKey::from_str("ce60ddcf96e4c4c3f94a305956a98de6afdebf59e8c6bd10b285b73b06949f08")?;
    let robust =
        ContentKey::from_str("ad842efc675c4225af9b269765ea8a0b6f2e79cbb03c6ed905753b8d0f210283")?;
    ui.set_history(vec![
        ui_record(
            invariant,
            "invariant",
            "不变的；恒定的",
            8,
            true,
            "TokenHub",
        ),
        ui_record(
            robust,
            "speaker-invariant representation",
            "说话人不变表征",
            3,
            false,
            "TokenHub",
        ),
    ]);
    ui.set_favorites(vec![ui_record(
        invariant,
        "invariant",
        "不变的；恒定的",
        8,
        true,
        "Pending sync",
    )]);
    ui.set_devices(vec![DeviceRecord {
        id: "device-fixture".into(),
        name: "Developer Mac".into(),
        platform: "macOS 15 arm64".into(),
        last_seen: "Now".into(),
        current: true,
        revoked: false,
    }]);
    ui.main_window().set_current_device("Developer Mac".into());
    ui.main_window().set_sync_status("Idle · 1 pending".into());

    let scenario = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ready".to_owned());
    let card = match scenario.as_str() {
        "loading" => LookupCardState::Loading {
            generation: 1,
            source: "invariant".to_owned(),
        },
        "error" => LookupCardState::Error {
            generation: 1,
            source: "invariant".to_owned(),
            kind: LookupCardErrorKind::ProviderConfigurationRequired,
        },
        "text" => LookupCardState::Ready {
            generation: 1,
            content_key: robust,
            source: "The representation should remain invariant to speaker identity.".to_owned(),
            translation: "该表征应对说话人身份保持不变。".to_owned(),
            favorite: false,
            effective_query_count: 2,
        },
        _ => LookupCardState::Ready {
            generation: 1,
            content_key: invariant,
            source: "invariant".to_owned(),
            translation: "不变的；恒定的".to_owned(),
            favorite: true,
            effective_query_count: 8,
        },
    };
    ui.show_main_window()?;
    ui.show_lookup_card(&card)?;
    slint::run_event_loop_until_quit()?;
    Ok(())
}
