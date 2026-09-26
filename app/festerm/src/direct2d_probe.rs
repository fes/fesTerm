//! Opt-in render-stage experiment; not linked into the application.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use eframe::{egui, egui_wgpu, wgpu};
use egui::epaint::{ClippedPrimitive, Primitive, TextureId};
use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use festerm_core::{Dimensions, Terminal};
use festerm_ui_egui::{install_terminal_fonts, EncodedInputSink, TerminalView};
use serde_json::json;

struct Sink;
impl EncodedInputSink for Sink {
    fn record_encoded_input(&mut self, _: &[u8]) {}
}

struct Scene {
    context: egui::Context,
    terminal: Terminal,
    view: TerminalView,
    textures: BTreeMap<u64, egui::ColorImage>,
    visual: bool,
}

impl Scene {
    fn frame(
        &mut self,
        state: &egui_wgpu::RenderState,
        screen: &egui_wgpu::ScreenDescriptor,
    ) -> Vec<ClippedPrimitive> {
        let context = self.context.clone();
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(
                        screen.size_in_pixels[0] as f32,
                        screen.size_in_pixels[1] as f32,
                    ) / screen.pixels_per_point,
                )),
                ..Default::default()
            },
            |ui| {
                if self.visual {
                    ui.set_clip_rect(ui.clip_rect().shrink2(egui::vec2(3.0, 2.0)));
                }
                self.view.show(ui, &mut self.terminal, &mut Sink);
                if self.visual {
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(egui::pos2(45.0, 45.0), egui::vec2(140.0, 70.0)),
                        6.0,
                        egui::Color32::from_rgba_unmultiplied(200, 100, 50, 96),
                    );
                }
            },
        );
        for (id, deltas) in &output.textures_delta.set {
            let TextureId::Managed(id_number) = *id else {
                panic!("probe does not support external textures");
            };
            for delta in deltas {
                let egui::ImageData::Color(image) = &delta.image;
                if let Some([x, y]) = delta.pos {
                    let target = self.textures.get_mut(&id_number).expect("existing atlas");
                    for row in 0..image.height() {
                        let offset = (y + row) * target.width() + x;
                        target.pixels[offset..offset + image.width()].copy_from_slice(
                            &image.pixels[row * image.width()..(row + 1) * image.width()],
                        );
                    }
                } else {
                    self.textures.insert(id_number, (**image).clone());
                }
                state
                    .renderer
                    .write()
                    .update_texture(&state.device, &state.queue, *id, delta);
            }
        }
        assert!(
            output.textures_delta.free.is_empty(),
            "unexpected texture retirement"
        );
        output.textures_delta.clear();
        context.tessellate(output.shapes, screen.pixels_per_point)
    }

    fn update(&mut self, case: &str, frame: usize) {
        let columns = self.terminal.dimensions().columns();
        let rows = self.terminal.dimensions().rows();
        let mut bytes = String::from("\x1b[?25l");
        if case == "sparse" {
            bytes.push_str(&format!(
                "\x1b[1;1HCPU probe frame {:010}\x1b[K",
                if frame == 0 {
                    1234567890u64
                } else {
                    9876543210
                }
            ));
        } else if case == "scrolling" && frame > 0 {
            bytes.push_str(&format!("\x1b[{};1H\r\nscroll 9876543210", rows));
        } else if case == "unicode" {
            bytes.push_str(
                "\x1b[HASCII 0123456789\r\nwide \u{754c} combining e\u{301}\r\n\
                 == != -> \u{1f916} \u{1f469}\u{200d}\u{1f52c}\r\n\
                 \x1b[4mUnderline\x1b[0m \x1b[9mStrike\x1b[0m\r\n\
                 \x1b[41;97m Color \x1b[0m\x1b[?25h",
            );
        } else {
            for row in 0..rows {
                bytes.push_str(&format!("\x1b[{};1H", row + 1));
                if case == "colored" {
                    bytes.push_str(&format!("\x1b[{};97m", 40 + row % 8));
                }
                for column in 0..columns.saturating_sub(1) {
                    bytes.push(char::from(b'!' + ((column + row + frame) % 90) as u8));
                }
                bytes.push_str("\x1b[0m");
            }
        }
        self.terminal.ingest(bytes.as_bytes());
    }
}

fn u32_to(file: &mut impl Write, value: u32) {
    file.write_all(&value.to_le_bytes()).unwrap();
}

fn f32_to(file: &mut impl Write, value: f32) {
    file.write_all(&value.to_le_bytes()).unwrap();
}

fn rect_to(file: &mut impl Write, rect: egui::Rect, scale: f32) {
    for value in [rect.min.x, rect.min.y, rect.max.x, rect.max.y] {
        f32_to(file, value * scale);
    }
}

fn export(
    path: &Path,
    scene: &Scene,
    frames: &[Vec<ClippedPrimitive>],
    screen: &egui_wgpu::ScreenDescriptor,
) {
    let mut file = BufWriter::new(File::create(path).unwrap());
    file.write_all(b"FESD2D01").unwrap();
    for dimension in screen.size_in_pixels {
        u32_to(&mut file, dimension);
    }
    u32_to(&mut file, scene.textures.len().try_into().unwrap());
    for (id, image) in &scene.textures {
        u32_to(&mut file, (*id).try_into().unwrap());
        u32_to(&mut file, image.width().try_into().unwrap());
        u32_to(&mut file, image.height().try_into().unwrap());
        for pixel in &image.pixels {
            file.write_all(&pixel.to_array()).unwrap();
        }
    }
    u32_to(&mut file, frames.len().try_into().unwrap());
    for frame in frames {
        u32_to(&mut file, frame.len().try_into().unwrap());
        for primitive in frame {
            rect_to(&mut file, primitive.clip_rect, screen.pixels_per_point);
            match &primitive.primitive {
                Primitive::Callback(callback) => {
                    // This isolated scene installs only the production solid-background callback.
                    u32_to(&mut file, 0);
                    rect_to(&mut file, callback.rect, screen.pixels_per_point);
                    file.write_all(&festerm_ui_egui::theme::SURFACE_TERMINAL.to_array())
                        .unwrap();
                }
                Primitive::Mesh(mesh) => {
                    u32_to(&mut file, 1);
                    let TextureId::Managed(id) = mesh.texture_id else {
                        panic!("unexpected native texture");
                    };
                    u32_to(&mut file, id.try_into().unwrap());
                    u32_to(&mut file, mesh.vertices.len().try_into().unwrap());
                    u32_to(&mut file, mesh.indices.len().try_into().unwrap());
                    for vertex in &mesh.vertices {
                        for value in [
                            vertex.pos.x * screen.pixels_per_point,
                            vertex.pos.y * screen.pixels_per_point,
                            vertex.uv.x,
                            vertex.uv.y,
                        ] {
                            f32_to(&mut file, value);
                        }
                        file.write_all(&vertex.color.to_array()).unwrap();
                    }
                    for index in &mesh.indices {
                        u32_to(&mut file, *index);
                    }
                }
            }
        }
    }
    file.flush().unwrap();
}

fn draw(
    state: &egui_wgpu::RenderState,
    target: &wgpu::Texture,
    frame: &[ClippedPrimitive],
    screen: &egui_wgpu::ScreenDescriptor,
) {
    let mut encoder = state.device.create_command_encoder(&Default::default());
    let mut renderer = state.renderer.write();
    let buffers = renderer.update_buffers(&state.device, &state.queue, &mut encoder, frame, screen);
    let view = target.create_view(&Default::default());
    {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            })
            .forget_lifetime();
        renderer.render(&mut pass, frame, screen);
    }
    state
        .queue
        .submit(buffers.into_iter().chain([encoder.finish()]));
    state
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(30)),
        })
        .expect("GPU completion");
}

fn capture(state: &egui_wgpu::RenderState, target: &wgpu::Texture) -> image::RgbaImage {
    let stride = (target.width() * 4).div_ceil(256) * 256;
    let buffer = state.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("probe readback"),
        size: u64::from(stride) * u64::from(target.height()),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = state.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(target.height()),
            },
        },
        target.size(),
    );
    state.queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
    state
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(30)),
        })
        .unwrap();
    receiver
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    let mapped = buffer
        .slice(..)
        .get_mapped_range()
        .expect("mapped readback");
    let mut image = image::RgbaImage::new(target.width(), target.height());
    for (y, row) in mapped.chunks_exact(stride as usize).enumerate() {
        for (x, pixel) in row[..target.width() as usize * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
        {
            image.put_pixel(
                x as u32,
                y as u32,
                image::Rgba([pixel[2], pixel[1], pixel[0], pixel[3]]),
            );
        }
    }
    drop(mapped);
    buffer.unmap();
    image
}

fn process_stats() -> serde_json::Value {
    let output = Command::new("powershell.exe").args([
        "-NoProfile", "-Command",
        &format!(
            "$p=Get-Process -Id {}; @{{cpu_ms=$p.TotalProcessorTime.TotalMilliseconds; \
             working_set=$p.WorkingSet64; private_bytes=$p.PrivateMemorySize64}} | ConvertTo-Json -Compress",
            std::process::id()
        ),
    ]).output().unwrap();
    assert!(output.status.success(), "process statistics failed");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[ignore = "Windows SDK Direct2D probe; run via optional validation"]
fn compare_direct2d_render_stage() {
    assert_eq!(
        std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
        Ok("1")
    );
    let directory = std::path::PathBuf::from(
        std::env::var_os("FESTERM_DIRECT2D_PROBE_DIR").expect("probe directory"),
    );
    let milliseconds: u64 = std::env::var("FESTERM_DIRECT2D_SAMPLE_MS")
        .unwrap_or_else(|_| "5000".into())
        .parse()
        .unwrap();
    assert!(milliseconds == 0 || (500..=30_000).contains(&milliseconds));
    let width: u32 = std::env::var("FESTERM_DIRECT2D_WIDTH")
        .unwrap_or_else(|_| "1920".into())
        .parse()
        .unwrap();
    let height: u32 = std::env::var("FESTERM_DIRECT2D_HEIGHT")
        .unwrap_or_else(|_| "1080".into())
        .parse()
        .unwrap();
    let scale: f32 = std::env::var("FESTERM_DIRECT2D_SCALE")
        .unwrap_or_else(|_| "1".into())
        .parse()
        .unwrap();
    assert!((320..=4096).contains(&width) && (200..=4096).contains(&height));
    assert!((1.0..=2.0).contains(&scale));
    let mut setup = default_wgpu_setup();
    let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
        unreachable!()
    };
    options.instance_descriptor.backends = wgpu::Backends::DX12;
    let mut state = create_render_state(setup, egui_wgpu::RendererOptions::default());
    assert_eq!(
        state.adapter.get_info().device_type,
        wgpu::DeviceType::Cpu,
        "this probe currently requires WARP"
    );
    state.target_format = wgpu::TextureFormat::Bgra8Unorm;
    *state.renderer.write() = egui_wgpu::Renderer::new(
        &state.device,
        state.target_format,
        egui_wgpu::RendererOptions::default(),
    );
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [width, height],
        pixels_per_point: scale,
    };
    let target = state.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("probe target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: state.target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    for case in ["sparse", "dense", "scrolling", "colored", "unicode"] {
        let context = egui::Context::default();
        context.set_pixels_per_point(scale);
        let generation = install_terminal_fonts(&context);
        let mut view = TerminalView::default();
        if case == "unicode" {
            view.set_font_set(festerm_ui_egui::TerminalFontSet::new(
                Default::default(),
                true,
                generation,
            ));
        }
        crate::software_background::install(&context, &state);
        let mut scene = Scene {
            context,
            terminal: Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap(),
            view,
            textures: BTreeMap::new(),
            visual: case == "unicode",
        };
        for _ in 0..3 {
            scene.frame(&state, &screen);
        }
        let mut frames = Vec::new();
        // Populate all glyphs before capturing either replay frame.
        for iteration in 0..4 {
            scene.update(case, iteration % 2);
            let frame = scene.frame(&state, &screen);
            if iteration >= 2 {
                frames.push(frame);
            }
        }
        let path = directory.join(format!("{case}.scene"));
        export(&path, &scene, &frames, &screen);
        if milliseconds != 0 {
            for index in 0..4 {
                draw(&state, &target, &frames[index % 2], &screen);
            }
        }
        let before = (milliseconds != 0).then(process_stats);
        let start = Instant::now();
        let mut times = Vec::new();
        while start.elapsed() < Duration::from_millis(milliseconds) {
            let frame_start = Instant::now();
            draw(&state, &target, &frames[times.len() % 2], &screen);
            times.push(frame_start.elapsed().as_secs_f64() * 1000.0);
            std::thread::sleep(Duration::from_millis(100).saturating_sub(frame_start.elapsed()));
        }
        let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
        let after = (milliseconds != 0).then(process_stats);
        assert!(milliseconds == 0 || times.len() >= 2);
        times.sort_by(f64::total_cmp);
        draw(&state, &target, &frames[0], &screen);
        let image = capture(&state, &target);
        let background = festerm_ui_egui::theme::SURFACE_TERMINAL.to_array();
        let non_background = image
            .pixels()
            .filter(|pixel| pixel.0 != background && pixel.0[3] > 0)
            .count();
        assert!(non_background > 100, "blank fixture");
        image
            .save(directory.join(format!("{case}-wgpu.png")))
            .unwrap();
        let mut result = json!({
            "case": case, "renderer": "egui-wgpu-with-240",
            "adapter": format!("{:?}", state.adapter.get_info()),
            "physical_size": [width, height], "scale": scale,
            "build": if cfg!(debug_assertions) { "debug" } else { "release" },
            "columns": scene.terminal.dimensions().columns(), "rows": scene.terminal.dimensions().rows(),
            "non_background_pixels": non_background,
        });
        if let (Some(before), Some(after)) = (before, after) {
            let cpu_ms = after["cpu_ms"].as_f64().unwrap() - before["cpu_ms"].as_f64().unwrap();
            let metrics = json!({
                "mode": "render-stage", "frames": times.len(), "wall_ms": wall_ms, "cpu_ms": cpu_ms,
                "cpu_percent": cpu_ms / wall_ms / std::thread::available_parallelism().unwrap().get() as f64 * 100.0,
                "target_hz": 10, "completed_hz": times.len() as f64 * 1000.0 / wall_ms,
                "cpu_ms_per_frame": cpu_ms / times.len() as f64,
                "mean_ms": times.iter().sum::<f64>() / times.len() as f64,
                "median_ms": times[times.len()/2], "p95_ms": times[times.len()*95/100],
                "memory": after,
            });
            result
                .as_object_mut()
                .unwrap()
                .extend(metrics.as_object().unwrap().clone());
        } else {
            result["mode"] = json!("capture-only");
        }
        std::fs::write(
            directory.join(format!("{case}-wgpu.json")),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("{result}");
    }
}
