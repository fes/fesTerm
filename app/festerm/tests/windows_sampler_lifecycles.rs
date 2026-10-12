#![cfg(all(windows, target_arch = "x86_64"))]

use std::sync::{Arc, Barrier};

use eframe::wgpu;

fn native_frame(device: &wgpu::Device, queue: &wgpu::Queue) {
    use eframe::egui;

    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(64.0, 64.0));
    let mut mesh = egui::Mesh::default();
    mesh.add_colored_rect(rect, egui::Color32::GREEN);
    let primitives = [egui::ClippedPrimitive {
        clip_rect: rect,
        primitive: egui::epaint::Primitive::Mesh(mesh),
    }];
    let textures = [(
        egui::TextureId::Managed(0),
        Arc::new(egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE])),
    )];
    let mut renderer =
        festerm_windows_direct2d::Renderer::new(device.clone(), queue.clone()).unwrap();
    let surface = renderer
        .render(
            rect,
            1.0,
            egui::Color32::BLACK,
            &primitives,
            &textures,
            None,
        )
        .unwrap()
        .expect("native diagnostic surface");
    drop(surface);
    drop(renderer);
}

fn egui_frame(
    runtime: &tokio::runtime::Runtime,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) {
    use eframe::{egui, egui_wgpu};
    use egui_kittest::TestRenderer;

    let setup = egui_wgpu::WgpuSetup::Existing(egui_wgpu::WgpuSetupExisting {
        instance: instance.clone(),
        adapter: adapter.clone(),
        device: device.clone(),
        queue: queue.clone(),
    });
    let state = runtime
        .block_on(egui_wgpu::RenderState::create(
            &egui_wgpu::WgpuConfiguration {
                wgpu_setup: setup,
                ..Default::default()
            },
            instance,
            None,
            egui_wgpu::RendererOptions::PREDICTABLE,
        ))
        .expect("egui diagnostic render state");
    let mut renderer = egui_kittest::wgpu::WgpuTestRenderer::from_render_state(state);
    let context = egui::Context::default();
    for _ in 0..2 {
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(160.0, 96.0),
                )),
                ..Default::default()
            },
            |ui| {
                ui.heading("Owned lifecycle probe");
                ui.label("Font texture upload and GPU readback");
            },
        );
        renderer.handle_delta(&mut output.textures_delta);
        let image = renderer.render(&context, &output).unwrap();
        assert_eq!(image.dimensions(), (160, 96));
        assert!(image.pixels().any(|pixel| pixel.0[3] != 0));
    }
}

fn parse_bound(name: &str, value: &str, maximum: usize) -> usize {
    let value = value.parse::<usize>().expect("numeric diagnostic setting");
    assert!((1..=maximum).contains(&value), "{name} out of bounds");
    value
}

fn bounded_variable(name: &str, default: usize, maximum: usize) -> usize {
    match std::env::var(name) {
        Ok(value) => parse_bound(name, &value, maximum),
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => panic!("{name}: {error}"),
    }
}

fn instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        flags: wgpu::InstanceFlags::default() | wgpu::InstanceFlags::VALIDATION,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

#[test]
fn diagnostic_settings_reject_invalid_or_unbounded_work() {
    assert_eq!(parse_bound("threads", "1", 32), 1);
    assert_eq!(parse_bound("threads", "32", 32), 32);
    assert_eq!(parse_bound("rounds", "128", 128), 128);
    for value in ["0", "33", "-1", "not-a-number", "999999999999999999999"] {
        assert!(std::panic::catch_unwind(|| parse_bound("threads", value, 32)).is_err());
    }
    assert!(std::panic::catch_unwind(|| parse_bound("rounds", "129", 128)).is_err());
}

#[test]
#[ignore = "bounded WARP native-exit investigation; use run-windows-sampler-diagnostics.ps1"]
fn concurrent_sampler_device_lifecycles() {
    assert_eq!(
        std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
        Ok("1")
    );
    assert_eq!(
        std::env::var("FESTERM_RUN_WINDOWS_SAMPLER_DIAGNOSTICS").as_deref(),
        Ok("1")
    );
    let threads = bounded_variable("FESTERM_SAMPLER_THREADS", 16, 32);
    let rounds = bounded_variable("FESTERM_SAMPLER_ROUNDS", 32, 128);
    let shared = match std::env::var("FESTERM_SAMPLER_INSTANCE").as_deref() {
        Ok("shared") => true,
        Ok("independent") => false,
        value => panic!("set FESTERM_SAMPLER_INSTANCE to shared or independent: {value:?}"),
    };
    let mixed = match std::env::var("FESTERM_SAMPLER_WORKLOAD").as_deref() {
        Ok("mixed") => true,
        Ok("sampler") => false,
        value => panic!("set FESTERM_SAMPLER_WORKLOAD to sampler or mixed: {value:?}"),
    };
    let shared_instance = shared.then(instance);
    let barrier = Arc::new(Barrier::new(threads));
    std::thread::scope(|scope| {
        for worker in 0..threads {
            let barrier = barrier.clone();
            let shared_instance = &shared_instance;
            scope.spawn(move || {
                barrier.wait();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("diagnostic runtime");
                for round in 0..rounds {
                    let instance = shared_instance.clone().unwrap_or_else(instance);
                    let adapter = runtime
                        .block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                            force_fallback_adapter: true,
                            ..Default::default()
                        }))
                        .expect("DX12 CPU adapter");
                    let info = adapter.get_info();
                    assert_eq!(info.backend, wgpu::Backend::Dx12);
                    assert_eq!(info.device_type, wgpu::DeviceType::Cpu);
                    let (device, queue) = runtime
                        .block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                        .expect("diagnostic device");
                    if mixed {
                        native_frame(&device, &queue);
                    }
                    let samplers: Vec<_> = (0..32)
                        .map(|index| {
                            device.create_sampler(&wgpu::SamplerDescriptor {
                                label: Some("bounded concurrent sampler probe"),
                                min_filter: wgpu::FilterMode::Linear,
                                mag_filter: wgpu::FilterMode::Linear,
                                lod_max_clamp: 32.0 + index as f32,
                                ..Default::default()
                            })
                        })
                        .collect();
                    if mixed {
                        egui_frame(&runtime, &instance, &adapter, &device, &queue);
                    }
                    queue.submit([]);
                    device
                        .poll(wgpu::PollType::wait_indefinitely())
                        .expect("completed diagnostic submission");
                    drop(samplers);
                    drop(queue);
                    drop(device);
                    drop(adapter);
                    drop(instance);
                    if round == rounds - 1 {
                        println!("completed worker={worker} rounds={rounds}");
                    }
                }
            });
        }
    });
    println!(
        "completed mode={} workload={} threads={threads} rounds={rounds}",
        if shared { "shared" } else { "independent" },
        if mixed { "mixed" } else { "sampler" },
    );
}
