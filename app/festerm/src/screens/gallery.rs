use super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Scene {
    Local,
    LocalError,
    SshSeparate,
    SshKey,
    SshCertificate,
    SshDurable,
    SshInvalid,
    SftpTextMode,
    SerialInvalid,
    LauncherNoMatch,
    LauncherCollapsed,
}

pub(crate) fn install(context: &egui::Context, tab: TabId, scene: Scene) {
    let mut state = LauncherState::default();
    state.ssh.quick_connect = "builder@build.example.com:2222".into();
    state.ssh.host = "build.example.com".into();
    state.ssh.username = "builder".into();
    state.ssh.port = "2222".into();
    match scene {
        Scene::Local | Scene::LocalError => {
            state.local_open = true;
            state.local = LocalLauncherForm {
                executable: "/bin/sh".into(),
                arguments: "-l".into(),
                working_directory: "/work/project".into(),
                feedback: matches!(scene, Scene::LocalError)
                    .then(|| "Enter a non-empty executable.".into()),
                initialized: true,
                focus_working_directory: false,
            };
        }
        Scene::SftpTextMode => {
            state.sftp_open = true;
            state.sftp = state.ssh.clone();
            state.sftp.sftp_gui_mode = false;
        }
        Scene::SerialInvalid => {
            state.serial_open = true;
            state.serial.device = "/dev/tty.fixture".into();
            state.serial.baud_rate = "invalid".into();
            state.serial.feedback = Some("Baud rate must be a positive number".into());
        }
        Scene::LauncherNoMatch => state.profile_search = "no-matching-profile".into(),
        Scene::LauncherCollapsed => {
            state.festerm_group_expanded = false;
            state.tmux_group_expanded = false;
            state.screen_group_expanded = false;
        }
        _ => {
            state.ssh_open = true;
            match scene {
                Scene::SshSeparate => state.ssh.destination_expanded = true,
                Scene::SshKey => {
                    state.ssh.authentication_method = SshAuthenticationMethod::PrivateKey
                }
                Scene::SshCertificate => {
                    state.ssh.authentication_method = SshAuthenticationMethod::Certificate
                }
                Scene::SshDurable => {
                    state.ssh.durable_session.enabled = true;
                    state.ssh.durable_session.provider_touched = true;
                    state.ssh.durable_session.automatic_recovery = true;
                }
                Scene::SshInvalid => {
                    state.ssh.feedback = Some("Host and username are required.".into())
                }
                _ => {}
            }
        }
    }
    context.data_mut(|data| data.insert_temp(launcher_state_id(tab), state));
}
