#!/usr/bin/env sh
# Reports every graphics adapter wgpu can see on this machine, which one the
# OS/driver default would pick, and which one egui_kittest's snapshot test
# harness actually forces (it deliberately prefers CPU/software rasterizers
# for cross-machine determinism; see egui_kittest's `native_adapter_selector`
# in its `wgpu.rs`). Useful when a P3/P6 visual-snapshot failure needs to be
# explained as an adapter/renderer difference rather than a code regression.
#
# This does not touch the workspace Cargo.toml/Cargo.lock: it builds a
# throwaway probe crate in a temp directory, pinned to the same `wgpu`
# version already resolved in the workspace lockfile, and removes it when
# done.
set -eu

repo_root=$(cd "$(dirname "$0")/.." && pwd)
lock_file="$repo_root/Cargo.lock"

wgpu_version=$(awk '
  /^name = "wgpu"$/ { found=1; next }
  found && /^version = / { gsub(/"/, "", $0); print $3; exit }
' "$lock_file")

if [ -z "${wgpu_version:-}" ]; then
    echo "error: could not find a pinned wgpu version in $lock_file" >&2
    exit 1
fi

probe_dir=$(mktemp -d)
trap 'rm -rf "$probe_dir"' EXIT

mkdir -p "$probe_dir/src"

cat >"$probe_dir/Cargo.toml" <<EOF
[package]
name = "wgpu-adapter-probe"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
wgpu = "$wgpu_version"
pollster = "0.4"
EOF

cat >"$probe_dir/src/main.rs" <<'EOF'
fn main() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));

    println!("All adapters wgpu can see on this machine:");
    for a in &adapters {
        let info = a.get_info();
        println!(
            "  name={:<45} backend={:<8?} device_type={:?}",
            info.name, info.backend, info.device_type
        );
    }

    match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) {
        Ok(a) => {
            let info = a.get_info();
            println!(
                "\nDefault adapter (what a real app / P4 native smoke would use):\n  name={} backend={:?} device_type={:?}",
                info.name, info.backend, info.device_type
            );
        }
        Err(e) => println!("\nDefault adapter: none available ({e:?})"),
    }

    // Mirrors egui_kittest's native_adapter_selector: prefer CPU, then
    // discrete GPU, then everything else (integrated/virtual/other).
    let mut kittest_order = adapters.iter().collect::<Vec<_>>();
    kittest_order.sort_by_key(|a| match a.get_info().backend {
        wgpu::Backend::Metal => 0,
        wgpu::Backend::Vulkan => 1,
        wgpu::Backend::Dx12 => 2,
        wgpu::Backend::Gl => 4,
        wgpu::Backend::BrowserWebGpu => 6,
        wgpu::Backend::Noop => 7,
    });
    kittest_order.sort_by_key(|a| match a.get_info().device_type {
        wgpu::DeviceType::Cpu => 0,
        wgpu::DeviceType::DiscreteGpu => 1,
        wgpu::DeviceType::Other
        | wgpu::DeviceType::IntegratedGpu
        | wgpu::DeviceType::VirtualGpu => 2,
    });

    match kittest_order.first() {
        Some(a) => {
            let info = a.get_info();
            println!(
                "\negui_kittest snapshot-test adapter (P3/P6):\n  name={} backend={:?} device_type={:?}",
                info.name, info.backend, info.device_type
            );
        }
        None => println!("\negui_kittest snapshot-test adapter: none available"),
    }
}
EOF

(cd "$probe_dir" && cargo run --quiet --release)
