#![cfg(feature = "relay")]

// Compile the setup binary's module without registering any real service.
#[path = "../src/service_install.rs"]
#[allow(dead_code)]
mod service_install;
