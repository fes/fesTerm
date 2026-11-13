//! Offline adapters for the production remote-service dialogs.

use super::*;

#[cfg(feature = "experimental-dev-tunnels")]
pub(crate) fn show(app: &mut FesTermApp, context: &egui::Context, surface: &str) {
    match surface {
        "host" => app.show_host_browser(context, false),
        "tunnels" => app.show_tunnel_browser(context, false),
        "connect" => app.show_native_tunnel(context, false),
        "hosting" => app.poll_local_hosts(context),
        _ => unreachable!("unknown gallery surface"),
    }
}

pub(crate) fn recording_status(ui: &mut egui::Ui, scene: &str) {
    let mut app = FesTermApp::for_test_with_configuration(festerm_config::Configuration::empty());
    app.recording_view.label = Some(
        match scene {
            "active" => "Recording terminal output · synthetic session",
            "preparing" => "Preparing private recording storage…",
            "exporting" => "Exporting recording · synthetic status",
            "failed" => "Recording ended incompletely; retained private source needs review",
            "complete" => "Recording is ready for export",
            _ => unreachable!("unknown recording status"),
        }
        .into(),
    );
    app.recording_view.active = (scene == "active").then(|| {
        festerm_recording::RecordingId::parse("11111111111141118111111111111111").unwrap()
    });
    app.recording_view.preparing = scene == "preparing";
    app.recording_view.exporting = scene == "exporting";
    app.recording_view.busy = scene == "preparing" || scene == "exporting";
    app.show_recording_status(ui);
}
