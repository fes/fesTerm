//! The exported Android activity entry point; application policy stays shared.

#[cfg(all(
    target_os = "android",
    feature = "native-recording-validation",
    debug_assertions
))]
mod recording_validation;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    if let Err(error) = festerm_android_platform::initialize(&app) {
        eprintln!("fesTerm: Android platform initialization failed: {error}");
        return;
    }
    if let Err(error) = festerm_mobile::run_native(app) {
        eprintln!("fesTerm: native host failed: {error}");
    }
    festerm_android_platform::shutdown();
}
