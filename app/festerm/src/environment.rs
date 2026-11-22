//! GUI facade for the shared per-child login environment correction.

use festerm_pty::LocalProfile;

pub fn with_corrected_local_path(profile: LocalProfile) -> LocalProfile {
    #[cfg(target_os = "macos")]
    if let (Some(shell), Some(home)) = (std::env::var_os("SHELL"), std::env::var_os("HOME")) {
        return festerm_pty::login_environment::with_corrected_local_path(
            profile,
            &shell,
            std::path::Path::new(&home),
        );
    }
    profile
}
