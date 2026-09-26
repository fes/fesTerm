fn main() {
    println!("cargo:rerun-if-changed=native/renderer.cpp");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
    {
        cc::Build::new()
            .cpp(true)
            .std("c++20")
            .flag("/EHsc")
            .file("native/renderer.cpp")
            .compile("festerm_direct2d");
        for library in ["d2d1", "d3d11", "dxgi"] {
            println!("cargo:rustc-link-lib={library}");
        }
    }
}
