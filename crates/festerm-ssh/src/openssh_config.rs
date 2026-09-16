//! Bounded, safe importer for the OpenSSH-config subset fesTerm supports.
//!
//! Only `Host`, `HostName`, `Port`, `User`, and `IdentityFile` are considered.
//! The parser does not expand shells, environment variables, or `~`; it does
//! not process `Include`, run commands, or open identity files.

use std::fmt;

use crate::{validate_username, HostIdentity, IdentityFileMetadata, ImportedSshProfile};

const MAX_OPENSSH_CONFIG_BYTES: usize = 128 * 1024;
const MAX_OPENSSH_CONFIG_LINES: usize = 2_048;
const MAX_OPENSSH_CONFIG_LINE_BYTES: usize = 4 * 1024;
const MAX_OPENSSH_CONFIG_TOKENS: usize = 16;
const MAX_IMPORTED_SSH_PROFILES: usize = 256;
const MAX_OPENSSH_IMPORT_DIAGNOSTICS: usize = 256;
const DEFAULT_SSH_PORT: u16 = 22;
const MAX_IDENTITY_FILE_METADATA_BYTES: usize = 4 * 1024;

/// Severity of a safe OpenSSH-import diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenSshConfigDiagnosticSeverity {
    Warning,
    Error,
}

/// A structured reason why an OpenSSH setting was not imported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenSshConfigDiagnosticKind {
    ConfigTooLarge { maximum: usize, actual: usize },
    TooManyLines { maximum: usize },
    LineTooLong { maximum: usize },
    TooManyTokens { maximum: usize },
    UnterminatedQuote,
    InvalidDirectiveSyntax,
    UnsupportedDirective { directive: String },
    DirectiveOutsideHost { directive: String },
    DirectiveInUnsupportedMatch { directive: String },
    MultipleHostPatterns,
    NegatedHostPattern,
    WildcardHostPattern,
    InvalidHostAlias,
    DuplicateHostAlias { alias: String },
    DuplicateDirective { directive: String },
    InvalidValue { directive: String },
    TooManyProfiles { maximum: usize },
    DiagnosticLimitReached { maximum: usize },
}

impl fmt::Display for OpenSshConfigDiagnosticKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigTooLarge { maximum, actual } => {
                write!(formatter, "config is {actual} bytes; maximum is {maximum}")
            }
            Self::TooManyLines { maximum } => {
                write!(formatter, "config exceeds the maximum of {maximum} lines")
            }
            Self::LineTooLong { maximum } => {
                write!(
                    formatter,
                    "config line exceeds the maximum of {maximum} bytes"
                )
            }
            Self::TooManyTokens { maximum } => {
                write!(
                    formatter,
                    "config line exceeds the maximum of {maximum} tokens"
                )
            }
            Self::UnterminatedQuote => formatter.write_str("config line has an unterminated quote"),
            Self::InvalidDirectiveSyntax => formatter.write_str("config line has invalid syntax"),
            Self::UnsupportedDirective { directive } => {
                write!(formatter, "unsupported OpenSSH directive {directive}")
            }
            Self::DirectiveOutsideHost { directive } => {
                write!(
                    formatter,
                    "OpenSSH directive {directive} appears outside an exact Host"
                )
            }
            Self::DirectiveInUnsupportedMatch { directive } => {
                write!(
                    formatter,
                    "OpenSSH directive {directive} appears in an unsupported Match section"
                )
            }
            Self::MultipleHostPatterns => {
                formatter.write_str("Host must contain exactly one exact alias")
            }
            Self::NegatedHostPattern => {
                formatter.write_str("negated Host patterns are not supported")
            }
            Self::WildcardHostPattern => {
                formatter.write_str("wildcard Host patterns are not supported")
            }
            Self::InvalidHostAlias => formatter.write_str("Host alias is not a simple exact alias"),
            Self::DuplicateHostAlias { alias } => {
                write!(
                    formatter,
                    "Host alias {alias} is ambiguous because it is duplicated"
                )
            }
            Self::DuplicateDirective { directive } => {
                write!(formatter, "OpenSSH directive {directive} is duplicated")
            }
            Self::InvalidValue { directive } => {
                write!(
                    formatter,
                    "OpenSSH directive {directive} has an invalid value"
                )
            }
            Self::TooManyProfiles { maximum } => {
                write!(
                    formatter,
                    "config exceeds the maximum of {maximum} imported profiles"
                )
            }
            Self::DiagnosticLimitReached { maximum } => {
                write!(
                    formatter,
                    "config diagnostics reached the maximum of {maximum}"
                )
            }
        }
    }
}

/// One source-positioned OpenSSH import diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenSshConfigDiagnostic {
    line: usize,
    severity: OpenSshConfigDiagnosticSeverity,
    kind: OpenSshConfigDiagnosticKind,
}

impl OpenSshConfigDiagnostic {
    pub const fn line(&self) -> usize {
        self.line
    }

    pub const fn severity(&self) -> OpenSshConfigDiagnosticSeverity {
        self.severity
    }

    pub fn kind(&self) -> &OpenSshConfigDiagnosticKind {
        &self.kind
    }
}

/// Bounded result of [`import_openssh_config`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenSshConfigImportReport {
    profiles: Vec<ImportedSshProfile>,
    diagnostics: Vec<OpenSshConfigDiagnostic>,
}

impl OpenSshConfigImportReport {
    pub fn profiles(&self) -> &[ImportedSshProfile] {
        &self.profiles
    }

    pub fn diagnostics(&self) -> &[OpenSshConfigDiagnostic] {
        &self.diagnostics
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == OpenSshConfigDiagnosticSeverity::Error)
    }
}

#[derive(Default)]
struct OpenSshHostBlock {
    alias: String,
    line: usize,
    hostname: Option<String>,
    port: Option<u16>,
    username: Option<String>,
    identity_file: Option<IdentityFileMetadata>,
    valid: bool,
}

impl OpenSshHostBlock {
    fn new(alias: String, line: usize) -> Self {
        Self {
            alias,
            line,
            valid: true,
            ..Self::default()
        }
    }
}

/// Imports the intentionally small, non-executing OpenSSH-config subset.
///
/// Only `Host`, `HostName`, `Port`, `User`, and `IdentityFile` are considered.
/// The parser does not expand shells, environment variables, or `~`; it does
/// not process `Include`, run commands, or open identity files. Any ambiguous,
/// unsupported, or unsafe directive is reported and never applied.
pub fn import_openssh_config(config: &str) -> OpenSshConfigImportReport {
    let mut diagnostics = Vec::new();
    if config.len() > MAX_OPENSSH_CONFIG_BYTES {
        push_openssh_diagnostic(
            &mut diagnostics,
            1,
            OpenSshConfigDiagnosticKind::ConfigTooLarge {
                maximum: MAX_OPENSSH_CONFIG_BYTES,
                actual: config.len(),
            },
        );
        return OpenSshConfigImportReport {
            profiles: Vec::new(),
            diagnostics,
        };
    }

    let mut blocks = Vec::new();
    let mut current_block = None;
    let mut configuration_incomplete = false;
    let mut in_unsupported_match = false;
    for (index, line) in config.split('\n').enumerate() {
        let line_number = index.saturating_add(1);
        if line_number > MAX_OPENSSH_CONFIG_LINES {
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::TooManyLines {
                    maximum: MAX_OPENSSH_CONFIG_LINES,
                },
            );
            configuration_incomplete = true;
            break;
        }
        if line.len() > MAX_OPENSSH_CONFIG_LINE_BYTES {
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::LineTooLong {
                    maximum: MAX_OPENSSH_CONFIG_LINE_BYTES,
                },
            );
            configuration_incomplete = true;
            continue;
        }
        let tokens = match tokenize_openssh_config_line(line) {
            Ok(tokens) => tokens,
            Err(kind) => {
                push_openssh_diagnostic(&mut diagnostics, line_number, kind);
                configuration_incomplete = true;
                continue;
            }
        };
        let Some((directive, values)) = split_openssh_directive(tokens) else {
            continue;
        };

        if directive == "host" {
            if let Some(block) = current_block.take() {
                blocks.push(block);
            }
            in_unsupported_match = false;
            match parse_exact_host_alias(&values) {
                Ok(alias) => current_block = Some(OpenSshHostBlock::new(alias, line_number)),
                Err(kind) => push_openssh_diagnostic(&mut diagnostics, line_number, kind),
            }
            continue;
        }

        if directive == "match" {
            if let Some(block) = current_block.take() {
                blocks.push(block);
            }
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::UnsupportedDirective { directive },
            );
            configuration_incomplete = true;
            in_unsupported_match = true;
            continue;
        }

        if in_unsupported_match {
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::DirectiveInUnsupportedMatch { directive },
            );
            configuration_incomplete = true;
            continue;
        }

        if directive == "include" && current_block.is_none() {
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::UnsupportedDirective { directive },
            );
            configuration_incomplete = true;
            continue;
        }

        let Some(block) = current_block.as_mut() else {
            push_openssh_diagnostic(
                &mut diagnostics,
                line_number,
                OpenSshConfigDiagnosticKind::DirectiveOutsideHost { directive },
            );
            configuration_incomplete = true;
            continue;
        };
        apply_openssh_host_directive(block, line_number, directive, values, &mut diagnostics);
    }
    if let Some(block) = current_block {
        blocks.push(block);
    }

    if configuration_incomplete {
        return OpenSshConfigImportReport {
            profiles: Vec::new(),
            diagnostics,
        };
    }
    build_imported_profiles(blocks, &mut diagnostics)
}

fn tokenize_openssh_config_line(line: &str) -> Result<Vec<String>, OpenSshConfigDiagnosticKind> {
    let mut tokens = Vec::new();
    let mut value = String::new();
    let mut quote = None;
    let mut token_started = false;
    let mut preceded_by_whitespace = true;

    for character in line.chars() {
        if let Some(quote_character) = quote {
            if character == quote_character {
                quote = None;
            } else {
                value.push(character);
            }
            token_started = true;
            preceded_by_whitespace = false;
            continue;
        }
        match character {
            '"' | '\'' => {
                quote = Some(character);
                token_started = true;
                preceded_by_whitespace = false;
            }
            '#' if preceded_by_whitespace => break,
            character if character.is_whitespace() => {
                if token_started {
                    push_openssh_token(&mut tokens, &mut value)?;
                    token_started = false;
                }
                preceded_by_whitespace = true;
            }
            _ => {
                value.push(character);
                token_started = true;
                preceded_by_whitespace = false;
            }
        }
    }
    if quote.is_some() {
        return Err(OpenSshConfigDiagnosticKind::UnterminatedQuote);
    }
    if token_started {
        push_openssh_token(&mut tokens, &mut value)?;
    }
    Ok(tokens)
}

fn push_openssh_token(
    tokens: &mut Vec<String>,
    value: &mut String,
) -> Result<(), OpenSshConfigDiagnosticKind> {
    if tokens.len() == MAX_OPENSSH_CONFIG_TOKENS {
        return Err(OpenSshConfigDiagnosticKind::TooManyTokens {
            maximum: MAX_OPENSSH_CONFIG_TOKENS,
        });
    }
    tokens.push(std::mem::take(value));
    Ok(())
}

fn split_openssh_directive(mut tokens: Vec<String>) -> Option<(String, Vec<String>)> {
    let first = tokens.first_mut()?;
    if let Some((directive, inline_value)) = first.split_once('=') {
        let directive = directive.to_ascii_lowercase();
        let inline_value = inline_value.to_owned();
        if inline_value.is_empty() {
            tokens.remove(0);
        } else {
            *first = inline_value;
        }
        Some((directive, tokens))
    } else {
        let directive = first.to_ascii_lowercase();
        tokens.remove(0);
        Some((directive, tokens))
    }
}

fn parse_exact_host_alias(values: &[String]) -> Result<String, OpenSshConfigDiagnosticKind> {
    if values.len() != 1 {
        return Err(OpenSshConfigDiagnosticKind::MultipleHostPatterns);
    }
    let alias = &values[0];
    if alias.starts_with('!') {
        return Err(OpenSshConfigDiagnosticKind::NegatedHostPattern);
    }
    if alias.contains(['*', '?', '[', ']']) {
        return Err(OpenSshConfigDiagnosticKind::WildcardHostPattern);
    }
    if !is_simple_host_alias(alias) {
        return Err(OpenSshConfigDiagnosticKind::InvalidHostAlias);
    }
    Ok(alias.to_ascii_lowercase())
}

fn is_simple_host_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn apply_openssh_host_directive(
    block: &mut OpenSshHostBlock,
    line: usize,
    directive: String,
    values: Vec<String>,
    diagnostics: &mut Vec<OpenSshConfigDiagnostic>,
) {
    if directive == "include" {
        block.valid = false;
        push_openssh_diagnostic(
            diagnostics,
            line,
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive },
        );
        return;
    }
    if !matches!(
        directive.as_str(),
        "hostname" | "port" | "user" | "identityfile"
    ) {
        block.valid = false;
        push_openssh_diagnostic(
            diagnostics,
            line,
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive },
        );
        return;
    }
    if values.len() != 1 {
        block.valid = false;
        push_openssh_diagnostic(
            diagnostics,
            line,
            OpenSshConfigDiagnosticKind::InvalidDirectiveSyntax,
        );
        return;
    }

    let value = &values[0];
    let duplicate = match directive.as_str() {
        "hostname" => block.hostname.is_some(),
        "port" => block.port.is_some(),
        "user" => block.username.is_some(),
        "identityfile" => block.identity_file.is_some(),
        _ => unreachable!("supported directive was checked above"),
    };
    if duplicate {
        block.valid = false;
        push_openssh_diagnostic(
            diagnostics,
            line,
            OpenSshConfigDiagnosticKind::DuplicateDirective { directive },
        );
        return;
    }

    match directive.as_str() {
        "hostname" if HostIdentity::new(value, DEFAULT_SSH_PORT).is_ok() => {
            block.hostname = Some(value.clone());
        }
        "port" => match value.parse::<u16>() {
            Ok(port) if port != 0 => block.port = Some(port),
            Ok(_) | Err(_) => {
                block.valid = false;
                push_openssh_diagnostic(
                    diagnostics,
                    line,
                    OpenSshConfigDiagnosticKind::InvalidValue { directive },
                );
            }
        },
        "user" if validate_username(value).is_ok() => block.username = Some(value.clone()),
        "identityfile"
            if !value.is_empty()
                && value.len() <= MAX_IDENTITY_FILE_METADATA_BYTES
                && !value.chars().any(char::is_control) =>
        {
            block.identity_file = Some(IdentityFileMetadata {
                path: value.clone(),
            });
        }
        _ => {
            block.valid = false;
            push_openssh_diagnostic(
                diagnostics,
                line,
                OpenSshConfigDiagnosticKind::InvalidValue { directive },
            );
        }
    }
}

fn build_imported_profiles(
    blocks: Vec<OpenSshHostBlock>,
    diagnostics: &mut Vec<OpenSshConfigDiagnostic>,
) -> OpenSshConfigImportReport {
    let mut duplicate_aliases = std::collections::BTreeSet::new();
    let mut seen_aliases = std::collections::BTreeSet::new();
    for block in &blocks {
        if !seen_aliases.insert(block.alias.clone()) {
            duplicate_aliases.insert(block.alias.clone());
        }
    }
    for block in &blocks {
        if duplicate_aliases.contains(&block.alias) {
            push_openssh_diagnostic(
                diagnostics,
                block.line,
                OpenSshConfigDiagnosticKind::DuplicateHostAlias {
                    alias: block.alias.clone(),
                },
            );
        }
    }

    let mut profiles = Vec::new();
    for block in blocks {
        if !block.valid || duplicate_aliases.contains(&block.alias) {
            continue;
        }
        if profiles.len() == MAX_IMPORTED_SSH_PROFILES {
            push_openssh_diagnostic(
                diagnostics,
                block.line,
                OpenSshConfigDiagnosticKind::TooManyProfiles {
                    maximum: MAX_IMPORTED_SSH_PROFILES,
                },
            );
            break;
        }
        let host = block.hostname.unwrap_or_else(|| block.alias.clone());
        let port = block.port.unwrap_or(DEFAULT_SSH_PORT);
        let Ok(identity) = HostIdentity::new(host, port) else {
            push_openssh_diagnostic(
                diagnostics,
                block.line,
                OpenSshConfigDiagnosticKind::InvalidValue {
                    directive: "hostname".to_owned(),
                },
            );
            continue;
        };
        profiles.push(ImportedSshProfile {
            host_alias: block.alias,
            identity,
            username: block.username,
            identity_file: block.identity_file,
        });
    }
    OpenSshConfigImportReport {
        profiles,
        diagnostics: std::mem::take(diagnostics),
    }
}

fn push_openssh_diagnostic(
    diagnostics: &mut Vec<OpenSshConfigDiagnostic>,
    line: usize,
    kind: OpenSshConfigDiagnosticKind,
) {
    if diagnostics.len() < MAX_OPENSSH_IMPORT_DIAGNOSTICS.saturating_sub(1) {
        diagnostics.push(OpenSshConfigDiagnostic {
            line,
            severity: OpenSshConfigDiagnosticSeverity::Error,
            kind,
        });
    } else if diagnostics.len() == MAX_OPENSSH_IMPORT_DIAGNOSTICS.saturating_sub(1) {
        diagnostics.push(OpenSshConfigDiagnostic {
            line,
            severity: OpenSshConfigDiagnosticSeverity::Error,
            kind: OpenSshConfigDiagnosticKind::DiagnosticLimitReached {
                maximum: MAX_OPENSSH_IMPORT_DIAGNOSTICS,
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openssh_import_handles_comments_quotes_and_alias_expansion() {
        let report = import_openssh_config(
            r#"
                # The literal identity path is metadata, not a file read.
                Host work # trailing comment
                    HostName "Example.COM"
                    Port=2200
                    User 'alice'
                    IdentityFile "$HOME/.ssh/work key" # no environment expansion
            "#,
        );

        assert!(!report.has_errors());
        assert!(report.diagnostics().is_empty());
        let profile = report.profiles().first().unwrap();
        assert_eq!(profile.host_alias(), "work");
        assert_eq!(profile.identity().host(), "example.com");
        assert_eq!(profile.identity().port(), 2200);
        assert_eq!(profile.username(), Some("alice"));
        assert_eq!(
            profile.identity_file().map(IdentityFileMetadata::path),
            Some("$HOME/.ssh/work key")
        );
    }

    #[test]
    fn openssh_import_uses_an_exact_alias_when_hostname_is_omitted() {
        let report = import_openssh_config(
            r#"
                Host build-server
                    User build
            "#,
        );

        assert!(!report.has_errors());
        let profile = report.profiles().first().unwrap();
        assert_eq!(profile.host_alias(), "build-server");
        assert_eq!(profile.identity().host(), "build-server");
        assert_eq!(profile.identity().port(), DEFAULT_SSH_PORT);
        assert_eq!(profile.username(), Some("build"));
    }

    #[test]
    fn openssh_import_reports_unsafe_directives_without_applying_them() {
        let report = import_openssh_config(
            r#"
                Host safe
                    HostName safe.example
                Host proxy
                    ProxyCommand ssh jump.example -W %h:%p
                Host multiplexed
                    ControlPath ~/.ssh/control-%r@%h:%p
            "#,
        );

        assert_eq!(report.profiles().len(), 1);
        assert_eq!(report.profiles()[0].host_alias(), "safe");
        assert!(report.has_errors());
        assert!(report.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive }
                if directive == "proxycommand"
        )));
        assert!(report.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive }
                if directive == "controlpath"
        )));
    }

    #[test]
    fn openssh_import_rejects_ambiguous_and_unprocessed_config_sections() {
        let wildcard = import_openssh_config("Host *.example\n    User alice\n");
        assert!(wildcard.profiles().is_empty());
        assert!(wildcard.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::WildcardHostPattern
        )));

        let duplicate = import_openssh_config(
            "Host one\n    HostName first.example\nHost one\n    HostName second.example\n",
        );
        assert!(duplicate.profiles().is_empty());
        assert!(duplicate.diagnostics().iter().all(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::DuplicateHostAlias { .. }
        )));

        let include = import_openssh_config("Include ~/.ssh/conf.d/*\nHost one\n");
        assert!(include.profiles().is_empty());
        assert!(include.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive }
                if directive == "include"
        )));

        let matched = import_openssh_config("Match host one\n    User alice\nHost one\n");
        assert!(matched.profiles().is_empty());
        assert!(matched.diagnostics().iter().any(|diagnostic| matches!(
            diagnostic.kind(),
            OpenSshConfigDiagnosticKind::UnsupportedDirective { directive }
                if directive == "match"
        )));
    }

    #[test]
    fn openssh_import_rejects_duplicate_directives_and_multiple_host_patterns() {
        let duplicate_directive = import_openssh_config("Host one\n    Port 22\n    Port 2200\n");
        assert!(duplicate_directive.profiles().is_empty());
        assert!(duplicate_directive
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(
                diagnostic.kind(),
                OpenSshConfigDiagnosticKind::DuplicateDirective { directive }
                    if directive == "port"
            )));

        let multiple_patterns = import_openssh_config("Host one two\n");
        assert!(multiple_patterns.profiles().is_empty());
        assert!(multiple_patterns
            .diagnostics()
            .iter()
            .any(|diagnostic| matches!(
                diagnostic.kind(),
                OpenSshConfigDiagnosticKind::MultipleHostPatterns
            )));
    }
}
