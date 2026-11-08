fn main() {
    println!("cargo:rerun-if-changed=native/encoder.m");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target.as_str(), "macos" | "ios") {
        cc::Build::new()
            .file("native/encoder.m")
            .flag("-fobjc-arc")
            .flag("-Wno-deprecated-declarations")
            .compile("festerm_recording_apple");
        for framework in [
            "AVFoundation",
            "CoreFoundation",
            "CoreMedia",
            "CoreVideo",
            "Foundation",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
