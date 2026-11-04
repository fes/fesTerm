#![cfg(feature = "relay")]

use festerm_tunnel_bridge::setup_storage;

#[path = "../src/test_support.rs"]
mod test_support;

// Compile the setup binary's module without registering any real service.
#[path = "../src/service_install.rs"]
#[allow(dead_code)]
mod service_install;
