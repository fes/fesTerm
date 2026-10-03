use super::{
    native,
    profile::{assert_same_pixels, draw, read_image, Sink},
};
use eframe::{egui, egui_wgpu};
use egui_kittest::{
    wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer},
    TestRenderer,
};
use egui_wgpu::wgpu;
use festerm_core::{Dimensions, Terminal};
use festerm_ui_egui::{NativePainterOptions, TerminalView};
use festerm_windows_direct2d::process_cpu_time;
use std::{
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

#[test]
#[ignore = "opt-in paced WARP atlas-cache mechanism control; not native-window latency"]
fn profile_native_font_atlas_capture() {
    assert_eq!(
        std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
        Ok("1")
    );
    let directory = PathBuf::from(
        std::env::var_os("FESTERM_FONT_ATLAS_PROFILE_OUT")
            .expect("set FESTERM_FONT_ATLAS_PROFILE_OUT"),
    );
    assert!(
        !directory.exists(),
        "preserve prior attempts; use a fresh output directory"
    );
    std::fs::create_dir_all(&directory).unwrap();
    let mut setup = default_wgpu_setup();
    let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
        unreachable!()
    };
    options.instance_descriptor.backends = wgpu::Backends::DX12;
    let mut state = create_render_state(setup, Default::default());
    assert_eq!(state.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    state.target_format = wgpu::TextureFormat::Bgra8Unorm;
    *state.renderer.write() =
        egui_wgpu::Renderer::new(&state.device, state.target_format, Default::default());
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [256, 128],
        pixels_per_point: 1.0,
    };
    let target = state.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("font atlas profile"),
        size: wgpu::Extent3d {
            width: 256,
            height: 128,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: state.target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let interval = Duration::from_millis(50);
    let frames = 200u32;
    let mut results = Vec::new();
    for grown in [false, true] {
        let mut reference = None;
        for repeat in 0..2 {
            for cached in if repeat == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                let context = egui::Context::default();
                context.set_visuals(festerm_ui_egui::theme::default_visuals());
                crate::software_background::install(&context, &state);
                let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
                let status = native::install_with_painter_options(
                    &context,
                    &state,
                    false,
                    NativePainterOptions {
                        capture_font_atlas_timings: true,
                        font_atlas_cache_budget_bytes: if cached { 64 * 1024 * 1024 } else { 0 },
                    },
                )
                .unwrap();
                let mut terminal = Terminal::new(Dimensions::new(30, 6).unwrap()).unwrap();
                terminal.ingest(
                    b"Controlled atlas capture\r\nABCDEFGHIJKLMNOPQRSTUVWXYZ\r\nChanging: 0",
                );
                let mut view = TerminalView::default();
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(256.0, 128.0),
                    )),
                    max_texture_side: Some(8192),
                    ..Default::default()
                };
                let mut step = |index: u32, grow: bool| {
                    terminal.ingest(format!("\x1b[3;11H{}", index % 2).as_bytes());
                    let mut output = context.run_ui(input.clone(), |ui| {
                        if grow {
                            let text: String = (' '..='~').collect();
                            for size in (14..128).step_by(2) {
                                let _ = ui.painter().layout_no_wrap(
                                    text.clone(),
                                    egui::FontId::monospace(size as f32),
                                    egui::Color32::WHITE,
                                );
                                let size = ui.ctx().fonts(|fonts| fonts.font_image_size());
                                if size[0] * size[1] * 4 >= 8 * 1024 * 1024 {
                                    break;
                                }
                            }
                        }
                        view.show(ui, &mut terminal, &mut Sink);
                    });
                    renderer.handle_delta(&mut output.textures_delta);
                    let primitives = context.tessellate(output.shapes, context.pixels_per_point());
                    assert!(
                        !draw(&state, &target, &screen, &primitives),
                        "host copy is not this experiment"
                    );
                };
                for index in 0..10 {
                    step(index, grown && index == 1);
                }
                status.font_atlas_samples.lock().unwrap().clear();
                let cpu_start = process_cpu_time().unwrap();
                let started = Instant::now();
                let mut late_frames = 0;
                for index in 0..frames {
                    step(index, false);
                    let deadline = interval * (index + 1);
                    late_frames += usize::from(started.elapsed() > deadline);
                    std::thread::sleep(deadline.saturating_sub(started.elapsed()));
                }
                let wall = started.elapsed();
                let cpu = process_cpu_time().unwrap() - cpu_start;
                let samples = status.font_atlas_samples.lock().unwrap();
                let image = read_image(&state, &target);
                let name = format!("grown-{grown}-cached-{cached}-repeat-{repeat}");
                image.save(directory.join(format!("{name}.png"))).unwrap();
                let bytes = samples
                    .first()
                    .expect("native capture samples")
                    .0
                    .atlas_bytes;
                let copies: usize = samples
                    .iter()
                    .map(|(capture, _)| capture.cloned_bytes)
                    .sum();
                let uploads: usize = samples.iter().map(|(_, uploads)| uploads).sum();
                let capture_ms = samples
                    .iter()
                    .map(|(capture, _)| capture.elapsed.as_secs_f64() * 1000.0)
                    .sum::<f64>();
                let valid = samples.len() == frames as usize
                    && late_frames == 0
                    && wall <= interval * frames + Duration::from_millis(100)
                    && samples.iter().all(|(capture, _)| {
                        capture.atlas_bytes == bytes && capture.reused == cached
                    })
                    && copies == if cached { 0 } else { bytes * frames as usize }
                    && uploads == 0
                    && status.active.load(Ordering::Relaxed)
                    && (!grown || bytes >= 8 * 1024 * 1024);
                results.push(serde_json::json!({
                    "case":name,"valid":false,"mechanism_sample_valid":valid,"pixels_checked":false,
                    "frames":frames,"late_frames":late_frames,
                    "viewport_pixels":[256,128],"terminal_grid":[30,6],
                    "atlas_bytes":bytes,"cloned_bytes":copies,"uploaded_textures":uploads,
                    "capture_ms_per_frame":capture_ms/f64::from(frames),
                    "cpu_ms_per_frame":cpu.as_secs_f64()*1000.0/f64::from(frames),
                    "wall_seconds":wall.as_secs_f64(),"fps":f64::from(frames)/wall.as_secs_f64(),
                }));
                std::fs::write(
                    directory.join("results.json"),
                    serde_json::to_vec_pretty(&results).unwrap(),
                )
                .unwrap();
                if let Some(reference) = &reference {
                    assert_same_pixels(reference, &image, "cache mode changed final pixels");
                } else {
                    reference = Some(image);
                }
                let result = results.last_mut().unwrap();
                result["pixels_checked"] = serde_json::json!(true);
                result["valid"] = serde_json::json!(valid);
                std::fs::write(
                    directory.join("results.json"),
                    serde_json::to_vec_pretty(&results).unwrap(),
                )
                .unwrap();
                assert!(valid, "invalid sample {name}: {}", results.last().unwrap());
                festerm_ui_egui::remove_root_terminal_painter(&context);
            }
        }
    }
}
