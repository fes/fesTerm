//! Repository-owned golden-fixture support for terminal behavior.

use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
};

use festerm_core::{Dimensions, Terminal};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fixture {
    pub name: String,
    pub dimensions: Dimensions,
    pub input: Vec<u8>,
    pub expected_grid: Vec<String>,
    pub expected_cursor: (usize, usize),
    pub expected_replies: Vec<u8>,
}

pub fn discover_fixtures(directory: &Path) -> Result<Vec<PathBuf>, FixtureError> {
    let mut fixtures = Vec::new();
    collect_fixture_paths(directory, &mut fixtures)?;
    fixtures.sort();
    Ok(fixtures)
}

pub fn load_fixture(path: &Path) -> Result<Fixture, FixtureError> {
    let source = fs::read_to_string(path).map_err(|error| FixtureError::io(path, error))?;
    parse_fixture(path, &source)
}

pub fn assert_fixture(fixture: &Fixture) -> Result<(), FixtureAssertionError> {
    let mut terminal =
        Terminal::new(fixture.dimensions).map_err(|error| FixtureAssertionError {
            message: format!(
                "golden fixture `{}` could not create a terminal: {error}",
                fixture.name
            ),
        })?;
    terminal.ingest(&fixture.input);

    let actual_grid = (0..fixture.dimensions.rows())
        .map(|row| {
            terminal
                .row_text(row)
                .expect("row is within fixture dimensions")
        })
        .collect::<Vec<_>>();
    let actual_cursor = (terminal.cursor().column(), terminal.cursor().row());
    let actual_replies = terminal.drain_replies();

    if actual_grid != fixture.expected_grid
        || actual_cursor != fixture.expected_cursor
        || actual_replies != fixture.expected_replies
    {
        return Err(FixtureAssertionError {
            message: format!(
                "golden fixture `{}` failed\n\
             expected grid:\n{}\n\
             actual grid:\n{}\n\
             expected cursor: {:?}\n\
             actual cursor: {:?}\n\
             expected replies: {}\n\
             actual replies: {}",
                fixture.name,
                render_grid(&fixture.expected_grid),
                render_grid(&actual_grid),
                fixture.expected_cursor,
                actual_cursor,
                render_bytes(&fixture.expected_replies),
                render_bytes(&actual_replies),
            ),
        });
    }

    Ok(())
}

fn collect_fixture_paths(
    directory: &Path,
    fixtures: &mut Vec<PathBuf>,
) -> Result<(), FixtureError> {
    let entries = fs::read_dir(directory).map_err(|error| FixtureError::io(directory, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| FixtureError::io(directory, error))?;
        let path = entry.path();
        if path.is_dir() {
            collect_fixture_paths(&path, fixtures)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "fixture")
        {
            fixtures.push(path);
        }
    }
    Ok(())
}

fn parse_fixture(path: &Path, source: &str) -> Result<Fixture, FixtureError> {
    let mut name = None;
    let mut dimensions = None;
    let mut input = None;
    let mut expected_grid = Vec::new();
    let mut expected_cursor = None;
    let mut expected_replies = None;
    let mut in_grid = false;

    for (index, raw_line) in source.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line == "grid:" {
            in_grid = true;
            continue;
        }
        if in_grid && line.starts_with('-') {
            expected_grid.push(parse_quoted(path, line_number, line[1..].trim())?);
            continue;
        }
        in_grid = false;

        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| FixtureError::parse(path, line_number, "expected `key: value`"))?;
        match key.trim() {
            "name" => name = Some(value.trim().to_owned()),
            "size" => dimensions = Some(parse_dimensions(path, line_number, value.trim())?),
            "input" => input = Some(parse_quoted_bytes(path, line_number, value.trim())?),
            "cursor" => expected_cursor = Some(parse_cursor(path, line_number, value.trim())?),
            "replies" => {
                expected_replies = Some(parse_quoted_bytes(path, line_number, value.trim())?)
            }
            other => {
                return Err(FixtureError::parse(
                    path,
                    line_number,
                    format!("unknown fixture key `{other}`"),
                ));
            }
        }
    }

    let dimensions = dimensions.ok_or_else(|| FixtureError::parse(path, 0, "missing `size`"))?;
    let expected_cursor =
        expected_cursor.ok_or_else(|| FixtureError::parse(path, 0, "missing `cursor`"))?;
    let expected_replies =
        expected_replies.ok_or_else(|| FixtureError::parse(path, 0, "missing `replies`"))?;
    let fixture = Fixture {
        name: name.unwrap_or_else(|| path.display().to_string()),
        dimensions,
        input: input.ok_or_else(|| FixtureError::parse(path, 0, "missing `input`"))?,
        expected_grid,
        expected_cursor,
        expected_replies,
    };

    if fixture.expected_grid.len() != fixture.dimensions.rows()
        || fixture
            .expected_grid
            .iter()
            .any(|row| row.chars().count() != fixture.dimensions.columns())
    {
        return Err(FixtureError::parse(
            path,
            0,
            "grid rows must exactly match the declared dimensions",
        ));
    }
    if fixture.expected_cursor.0 >= fixture.dimensions.columns()
        || fixture.expected_cursor.1 >= fixture.dimensions.rows()
    {
        return Err(FixtureError::parse(
            path,
            0,
            "cursor must lie within the declared dimensions",
        ));
    }

    Ok(fixture)
}

fn parse_dimensions(path: &Path, line: usize, value: &str) -> Result<Dimensions, FixtureError> {
    let (columns, rows) = value
        .split_once('x')
        .ok_or_else(|| FixtureError::parse(path, line, "size must be `<columns>x<rows>`"))?;
    let columns = parse_usize(path, line, columns, "column count")?;
    let rows = parse_usize(path, line, rows, "row count")?;
    Dimensions::new(columns, rows)
        .map_err(|error| FixtureError::parse(path, line, error.to_string()))
}

fn parse_cursor(path: &Path, line: usize, value: &str) -> Result<(usize, usize), FixtureError> {
    let (column, row) = value
        .split_once(',')
        .ok_or_else(|| FixtureError::parse(path, line, "cursor must be `<column>,<row>`"))?;
    Ok((
        parse_usize(path, line, column, "cursor column")?,
        parse_usize(path, line, row, "cursor row")?,
    ))
}

fn parse_usize(path: &Path, line: usize, value: &str, label: &str) -> Result<usize, FixtureError> {
    value.trim().parse().map_err(|error| {
        FixtureError::parse(path, line, format!("invalid {label} `{value}`: {error}"))
    })
}

fn parse_quoted(path: &Path, line: usize, value: &str) -> Result<String, FixtureError> {
    String::from_utf8(parse_quoted_bytes(path, line, value)?)
        .map_err(|error| FixtureError::parse(path, line, format!("value must be UTF-8: {error}")))
}

fn parse_quoted_bytes(path: &Path, line: usize, value: &str) -> Result<Vec<u8>, FixtureError> {
    let unquoted = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| FixtureError::parse(path, line, "value must be double quoted"))?;
    unescape_bytes(path, line, unquoted)
}

fn unescape_bytes(path: &Path, line: usize, value: &str) -> Result<Vec<u8>, FixtureError> {
    let mut output = Vec::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            let mut encoded = [0; 4];
            output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            continue;
        }

        let escape = characters
            .next()
            .ok_or_else(|| FixtureError::parse(path, line, "incomplete escape sequence"))?;
        match escape {
            '\\' => output.push(b'\\'),
            '"' => output.push(b'"'),
            'n' => output.push(b'\n'),
            'r' => output.push(b'\r'),
            't' => output.push(b'\t'),
            'x' => {
                let hexadecimal = characters.by_ref().take(2).collect::<String>();
                let byte = u8::from_str_radix(&hexadecimal, 16).map_err(|error| {
                    FixtureError::parse(path, line, format!("invalid hexadecimal escape: {error}"))
                })?;
                output.push(byte);
            }
            other => {
                return Err(FixtureError::parse(
                    path,
                    line,
                    format!("unsupported escape `\\{other}`"),
                ));
            }
        }
    }
    Ok(output)
}

fn render_grid(grid: &[String]) -> String {
    grid.iter()
        .enumerate()
        .map(|(row, content)| format!("  {row:>3} |{content}|"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "\"\"".to_owned();
    }

    bytes
        .iter()
        .map(|byte| format!("\\x{byte:02X}"))
        .collect::<String>()
}

#[derive(Debug)]
pub struct FixtureError {
    message: String,
}

impl FixtureError {
    fn io(path: &Path, error: std::io::Error) -> Self {
        Self {
            message: format!("{}: {error}", path.display()),
        }
    }

    fn parse(path: &Path, line: usize, message: impl Into<String>) -> Self {
        let location = if line == 0 {
            path.display().to_string()
        } else {
            format!("{}:{line}", path.display())
        };
        Self {
            message: format!("{location}: {}", message.into()),
        }
    }
}

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for FixtureError {}

#[derive(Debug)]
pub struct FixtureAssertionError {
    message: String,
}

impl fmt::Display for FixtureAssertionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for FixtureAssertionError {}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::parse_fixture;

    #[test]
    fn hexadecimal_input_escapes_preserve_raw_bytes() {
        let fixture = parse_fixture(
            Path::new("raw-bytes.fixture"),
            r#"
size: 5x1
input: "\x80A\xC3\x28B\xC2\x9B"
grid:
  - "A(B  "
cursor: 3,0
replies: ""
"#,
        )
        .expect("fixture should parse");

        assert_eq!(fixture.input, [0x80, b'A', 0xc3, b'(', b'B', 0xc2, 0x9b]);
    }

    #[test]
    fn invalid_fixture_dimensions_are_readable_errors() {
        let error = parse_fixture(
            Path::new("invalid-size.fixture"),
            r#"
size: 1x1
input: ""
grid:
  - " "
cursor: 0,0
replies: ""
"#,
        )
        .expect_err("fixture should reject a one-column screen");

        assert!(error.to_string().contains("at least 2 columns"));
    }
}
