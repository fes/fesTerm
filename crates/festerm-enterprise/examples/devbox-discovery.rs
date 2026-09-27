use std::env;
use std::process::ExitCode;
use std::time::Duration;

use festerm_enterprise::{
    begin_authorization, AuthConfiguration, DevBox, DevCenterClient, DevCenterUri, EntraTenantId,
    OperationControl, OperationControlError, Project, ProjectName, PublicClientId,
};

const DEFAULT_TIMEOUT_SECS: u64 = 180;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_LINES: usize = 8192;

#[derive(Default)]
struct InventoryOutput {
    lines: Vec<String>,
    bytes: usize,
}

impl InventoryOutput {
    fn push(&mut self, line: String) -> Result<(), String> {
        let bytes = self
            .bytes
            .checked_add(line.len())
            .and_then(|bytes| bytes.checked_add(1))
            .filter(|bytes| *bytes <= MAX_OUTPUT_BYTES)
            .ok_or_else(|| "combined inventory exceeds the display byte limit".to_owned())?;
        if self.lines.len() >= MAX_OUTPUT_LINES {
            return Err("combined inventory exceeds the display line limit".to_owned());
        }
        self.lines.push(line);
        self.bytes = bytes;
        Ok(())
    }

    fn append_dev_boxes(&mut self, dev_boxes: &[DevBox]) -> Result<(), String> {
        if dev_boxes.is_empty() {
            return self.push("  No owned Dev Boxes were returned.".to_owned());
        }
        for dev_box in dev_boxes {
            let name = escape_control_text(&dev_box.name);
            self.push(match &dev_box.provisioning_state {
                Some(state) => format!("  - {name} ({})", escape_control_text(state)),
                None => format!("  - {name}"),
            })?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Options {
    tenant_id: String,
    client_id: String,
    dev_center_uri: String,
    project: Option<String>,
    timeout_secs: u64,
}

fn main() -> ExitCode {
    let options = match parse_options(env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("devbox-discovery: {message}");
            eprintln!(
                "usage: devbox-discovery --tenant-guid GUID --client-guid GUID --dev-center-uri URI [--project NAME] [--timeout-secs N]"
            );
            return ExitCode::from(2);
        }
    };

    match run(options) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("devbox-discovery: {error}");
            ExitCode::from(1)
        }
    }
}

fn parse_options<I: Iterator<Item = String>>(mut arguments: I) -> Result<Options, String> {
    let mut options = Options {
        tenant_id: String::new(),
        client_id: String::new(),
        dev_center_uri: String::new(),
        project: None,
        timeout_secs: DEFAULT_TIMEOUT_SECS,
    };

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--tenant-guid" => options.tenant_id = take_value(&mut arguments, "--tenant-guid")?,
            "--client-guid" => options.client_id = take_value(&mut arguments, "--client-guid")?,
            "--dev-center-uri" => {
                options.dev_center_uri = take_value(&mut arguments, "--dev-center-uri")?
            }
            "--project" => options.project = Some(take_value(&mut arguments, "--project")?),
            "--timeout-secs" => {
                options.timeout_secs = take_value(&mut arguments, "--timeout-secs")?
                    .parse()
                    .map_err(|_| "--timeout-secs must be a positive integer".to_owned())?;
                if options.timeout_secs == 0 {
                    return Err("--timeout-secs must be greater than zero".to_owned());
                }
            }
            _ => return Err("unrecognized command-line option".to_owned()),
        }
    }

    if options.tenant_id.is_empty()
        || options.client_id.is_empty()
        || options.dev_center_uri.is_empty()
    {
        return Err("--tenant-guid, --client-guid, and --dev-center-uri are required".to_owned());
    }
    if let Some(project) = options.project.as_deref() {
        ProjectName::parse(project).map_err(|error| error.to_string())?;
    }
    Ok(options)
}

fn take_value<I: Iterator<Item = String>>(arguments: &mut I, name: &str) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn run(options: Options) -> Result<Vec<String>, String> {
    let tenant_id = EntraTenantId::parse(&options.tenant_id).map_err(|error| error.to_string())?;
    let client_id = PublicClientId::parse(&options.client_id).map_err(|error| error.to_string())?;
    let dev_center_uri =
        DevCenterUri::parse(&options.dev_center_uri).map_err(|error| error.to_string())?;
    let control = OperationControl::with_timeout(Duration::from_secs(options.timeout_secs))
        .map_err(map_timeout_error)?;
    install_ctrlc(control.clone())?;

    eprintln!("Opening the system browser for transient sign-in.");
    eprintln!(
        "No broker/device-compliance proof or device-code fallback is implemented in this example."
    );
    let session = begin_authorization(
        AuthConfiguration::new(tenant_id, client_id),
        control.clone(),
    )
    .map_err(|error| error.to_string())?;
    let token = session
        .open_browser_and_complete()
        .map_err(|error| error.to_string())?;
    let client = DevCenterClient::new(dev_center_uri, token).map_err(|error| error.to_string())?;

    if control.is_cancelled() {
        return Err("operation cancelled".to_owned());
    }

    if let Some(project) = options.project {
        let project = ProjectName::parse(&project).map_err(|error| error.to_string())?;
        let dev_boxes = client
            .list_owned_dev_boxes(&project, &control)
            .map_err(|error| error.to_string())?;
        let mut output = InventoryOutput::default();
        output.push(format!("Project: {}", project.as_str()))?;
        output.append_dev_boxes(&dev_boxes)?;
        return Ok(output.lines);
    }

    let projects = client
        .list_projects(&control)
        .map_err(|error| error.to_string())?;
    render_multi_project_output(
        &projects,
        |project_name| {
            client
                .list_owned_dev_boxes(project_name, &control)
                .map_err(|error| error.to_string())
        },
        &control,
    )
}

fn render_multi_project_output(
    projects: &[Project],
    mut fetch_dev_boxes: impl FnMut(&ProjectName) -> Result<Vec<DevBox>, String>,
    control: &OperationControl,
) -> Result<Vec<String>, String> {
    if control.is_cancelled() {
        return Err("operation cancelled".to_owned());
    }
    if projects.is_empty() {
        return Ok(vec![
            "No projects were returned by the configured Dev Center.".to_owned(),
        ]);
    }

    let mut output = InventoryOutput::default();
    for project in projects {
        if control.is_cancelled() {
            return Err("operation cancelled".to_owned());
        }
        let project_name = ProjectName::parse(&project.name).map_err(|error| error.to_string())?;
        output.push(format!("Project: {}", project_name.as_str()))?;
        let dev_boxes = fetch_dev_boxes(&project_name)?;
        output.append_dev_boxes(&dev_boxes)?;
    }
    if control.is_cancelled() {
        return Err("operation cancelled".to_owned());
    }
    Ok(output.lines)
}

fn escape_control_text(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        if character.is_control() {
            escaped.push_str(&character.escape_default().to_string());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

fn install_ctrlc(control: OperationControl) -> Result<(), String> {
    ctrlc::set_handler(move || {
        control.cancel();
    })
    .map_err(|_| "failed to install Ctrl-C handler".to_owned())
}

fn map_timeout_error(error: OperationControlError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_enterprise::DevBox;
    use std::time::Instant;

    fn sample_project(name: &str) -> Project {
        Project {
            name: name.to_owned(),
            display_name: None,
            description: None,
            uri: "https://example.invalid/projects/sample".to_owned(),
            max_dev_boxes_per_user: None,
        }
    }

    fn sample_dev_box(name: &str, provisioning_state: Option<&str>) -> DevBox {
        DevBox {
            name: name.to_owned(),
            project_name: "Alpha01".to_owned(),
            provisioning_state: provisioning_state.map(str::to_owned),
            pool_name: None,
            location: None,
            os_type: None,
            user: None,
            uri: "https://example.invalid/devboxes/sample".to_owned(),
        }
    }

    #[test]
    fn parse_options_rejects_unknown_flags_without_echoing_values() {
        let error = parse_options(["--mystery=secret".to_owned()].into_iter())
            .err()
            .expect("unknown option should fail");
        assert_eq!(error, "unrecognized command-line option");
        assert!(!error.contains("secret"));
    }

    #[test]
    fn parse_options_rejects_zero_timeout() {
        let error = parse_options(
            [
                "--tenant-guid".to_owned(),
                "11111111-1111-1111-1111-111111111111".to_owned(),
                "--client-guid".to_owned(),
                "22222222-2222-2222-2222-222222222222".to_owned(),
                "--dev-center-uri".to_owned(),
                "https://1234-name.region.devcenter.azure.com".to_owned(),
                "--timeout-secs".to_owned(),
                "0".to_owned(),
            ]
            .into_iter(),
        )
        .err()
        .expect("zero timeout should fail");
        assert_eq!(error, "--timeout-secs must be greater than zero");
    }

    #[test]
    fn render_dev_boxes_escapes_control_characters() {
        let mut output = InventoryOutput::default();
        output
            .append_dev_boxes(&[sample_dev_box("Box001", Some("Ready\u{1b}[31m"))])
            .unwrap();
        assert_eq!(output.lines[0], "  - Box001 (Ready\\u{1b}[31m)");
    }

    #[test]
    fn parse_options_rejects_invalid_project_before_sign_in() {
        let result = parse_options(
            [
                "--tenant-guid",
                "11111111-1111-1111-1111-111111111111",
                "--client-guid",
                "22222222-2222-2222-2222-222222222222",
                "--dev-center-uri",
                "https://fixture.region.devcenter.azure.com",
                "--project",
                "invalid/project",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        assert!(result.is_err());
    }

    #[test]
    fn combined_inventory_enforces_byte_bounds_before_growing() {
        let mut output = InventoryOutput::default();
        output.push("x".repeat(MAX_OUTPUT_BYTES - 1)).unwrap();
        assert_eq!(output.bytes, MAX_OUTPUT_BYTES);
        assert!(output.push(String::new()).is_err());
        assert_eq!(output.lines.len(), 1);
        assert_eq!(output.bytes, MAX_OUTPUT_BYTES);
    }

    #[test]
    fn combined_inventory_bounds_apply_across_projects() {
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        let projects = ["Alpha01", "Beta02", "Gamma03", "Delta04", "Epsilon05"].map(sample_project);
        let mut fetched = 0;
        let result = render_multi_project_output(
            &projects,
            |_| {
                fetched += 1;
                Ok((0..2048).map(|_| sample_dev_box("Box001", None)).collect())
            },
            &control,
        );
        assert_eq!(
            result.unwrap_err(),
            "combined inventory exceeds the display line limit"
        );
        assert_eq!(fetched, 4);
    }

    #[test]
    fn render_multi_project_output_returns_nonzero_failure_on_partial_errors() {
        let control = OperationControl::with_deadline(Instant::now() + Duration::from_secs(5));
        let error = render_multi_project_output(
            &[sample_project("Alpha01"), sample_project("Beta02")],
            |project_name| {
                if project_name.as_str() == "Beta02" {
                    Err("network request failed".to_owned())
                } else {
                    Ok(vec![sample_dev_box("Box001", Some("Succeeded"))])
                }
            },
            &control,
        )
        .unwrap_err();
        assert_eq!(error, "network request failed");
    }

    #[test]
    fn render_multi_project_output_stops_promptly_on_cancel() {
        let control = OperationControl::with_deadline(Instant::now() + Duration::from_secs(5));
        control.cancel();
        let error =
            render_multi_project_output(&[sample_project("Alpha01")], |_| unreachable!(), &control)
                .unwrap_err();
        assert_eq!(error, "operation cancelled");
    }
}
