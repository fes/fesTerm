use festerm_config::{SshPortForwardConfiguration, SshPortForwardDirection};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PortForwardDraft {
    pub(crate) direction: SshPortForwardDirection,
    pub(crate) bind_host: String,
    pub(crate) bind_port: String,
    pub(crate) destination_host: String,
    pub(crate) destination_port: String,
}

impl Default for PortForwardDraft {
    fn default() -> Self {
        Self {
            direction: SshPortForwardDirection::Local,
            bind_host: "127.0.0.1".to_owned(),
            bind_port: String::new(),
            destination_host: String::new(),
            destination_port: String::new(),
        }
    }
}

impl PortForwardDraft {
    pub(crate) fn from_configuration(forward: &SshPortForwardConfiguration) -> Self {
        Self {
            direction: forward.direction(),
            bind_host: forward.bind_host().to_owned(),
            bind_port: forward.bind_port().to_string(),
            destination_host: forward.destination_host().to_owned(),
            destination_port: forward.destination_port().to_string(),
        }
    }

    pub(crate) fn build(&self) -> Result<SshPortForwardConfiguration, String> {
        let bind_port: u16 = self
            .bind_port
            .trim()
            .parse()
            .map_err(|_| "Bind port must be a number between 1 and 65535".to_owned())?;
        let destination_port: u16 = self
            .destination_port
            .trim()
            .parse()
            .map_err(|_| "Destination port must be a number between 1 and 65535".to_owned())?;
        SshPortForwardConfiguration::new(
            self.direction,
            self.bind_host.trim(),
            bind_port,
            self.destination_host.trim(),
            destination_port,
        )
        .map_err(|error| error.to_string())
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::PortForwardDraft;
    use festerm_config::SshPortForwardDirection;

    #[test]
    fn port_forward_draft_builds_the_same_validated_configuration_for_both_ui_flows() {
        let draft = PortForwardDraft {
            direction: SshPortForwardDirection::Remote,
            bind_host: "127.0.0.1".to_owned(),
            bind_port: "18080".to_owned(),
            destination_host: "localhost".to_owned(),
            destination_port: "8080".to_owned(),
        };

        let configuration = draft.build().expect("shared draft should validate");

        assert_eq!(configuration.direction(), SshPortForwardDirection::Remote);
        assert_eq!(configuration.bind_host(), "127.0.0.1");
        assert_eq!(configuration.bind_port(), 18080);
        assert_eq!(configuration.destination_host(), "localhost");
        assert_eq!(configuration.destination_port(), 8080);
    }
}
