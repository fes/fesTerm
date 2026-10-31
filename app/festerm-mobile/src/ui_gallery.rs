//! Review images of the actual mobile UI, never native iOS acceptance.
//! Only offline fixtures and a synthetic navigation gesture are used: no sockets,
//! credentials, clipboard reads or personal filesystem resources.

use super::*;
use egui_kittest::{kittest::Queryable, Harness};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, process::Command};

fn git_fact(arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("git source attribution");
    assert!(output.status.success(), "git source attribution failed");
    String::from_utf8(output.stdout)
        .expect("UTF-8 git attribution")
        .trim()
        .to_owned()
}

#[test]
#[ignore = "explicit screenshot generation requires a working headless wgpu adapter"]
fn capture_mobile_ui_gallery() {
    let output = PathBuf::from(
        std::env::var_os("FESTERM_MOBILE_UI_OUT")
            .expect("use scripts/capture-mobile-ui.py for fresh attributed output"),
    );
    // Never overwrite an earlier attempt, including a failed one.
    fs::create_dir(&output).expect("capture output must be new, with an existing parent");
    let source = git_fact(&["rev-parse", "HEAD"]);
    let dirty = !git_fact(&["status", "--porcelain", "--untracked-files=normal"]).is_empty();
    let mut images = Vec::new();
    for (family, width, height, occlusion) in [
        ("phone", 390.0, 844.0, 0.35),
        ("tablet", 834.0, 1194.0, 0.30),
    ] {
        for state in ["terminal", "connect", "arrow-helper"] {
            let lifecycle = Rc::new(Cell::new(Lifecycle {
                active: true,
                ..Default::default()
            }));
            let app = MobileApp::new(lifecycle);
            let mut harness = Harness::builder()
                .with_size(egui::vec2(width, height))
                .build_ui_state(
                    move |ui, app: &mut MobileApp| app.show_with_occlusion(ui, occlusion),
                    app,
                );
            // Pin the presentation preference, not the native platform theme.
            harness.ctx.set_theme(egui::ThemePreference::Dark);
            harness
                .ctx
                .all_styles_mut(|style| style.visuals.text_cursor.blink = false);
            harness.remove_cursor();
            harness.run();
            if state == "connect" {
                harness.get_by_label("Connect").click();
                harness.run();
            }
            if state == "arrow-helper" {
                let origin = harness.state().view.diagnostics().grid_rect.unwrap().center();
                let time = harness.ctx.input(|input| input.time);
                for (elapsed, phase, position) in [
                    (0.01, egui::TouchPhase::Start, origin),
                    (0.51, egui::TouchPhase::Move, origin),
                    (0.52, egui::TouchPhase::Move, origin + egui::vec2(0.0, -90.0)),
                ] {
                    harness.input_mut().time = Some(time + elapsed);
                    harness.event(egui::Event::Touch {
                        device_id: egui::TouchDeviceId(1),
                        id: egui::TouchId(1),
                        phase,
                        pos: position,
                        force: None,
                    });
                    harness.step();
                }
                assert_eq!(
                    harness.state().gestures.helper(),
                    Some(Some(terminal_gesture::Direction::Up))
                );
                // Keep the contact held for the image. Running to idle would
                // continue the real repeat scheduler rather than settle.
                harness.remove_cursor();
                harness.step();
            } else {
                harness.remove_cursor();
                harness.run();
            }
            let image = harness.render().expect("headless mobile rendering");
            let filename = format!("{family}-{state}.png");
            let path = output.join(&filename);
            image.save(&path).expect("save mobile review PNG");
            let hash: String = Sha256::digest(fs::read(&path).expect("read PNG"))
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            images.push(serde_json::json!({
                "image": filename,
                "state": state,
                "viewport_class": family,
                "logical_size": [width, height],
                "pixel_size": [image.width(), image.height()],
                "simulated_keyboard_occlusion_fraction": occlusion,
                "native_keyboard_rendered": false,
                "sha256": hash,
            }));
        }
    }
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "capture_method": "egui_kittest headless wgpu; actual MobileApp widgets",
            "source_commit": source,
            "source_dirty": dirty,
            "host_os": std::env::consts::OS,
            "native_ios": false,
            "native_validation": "not-run",
            "fixture": "repository-owned offline terminal, empty connection form and synthetic hold/drag; no SSH worker",
            "presentation": "Dark preference; nonblinking cursor; no system keyboard pixels",
            "images": images,
        }))
        .expect("serialize mobile capture manifest"),
    )
    .expect("save mobile capture manifest");
}
