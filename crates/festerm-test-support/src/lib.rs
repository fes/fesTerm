//! Repository-owned golden-fixture support for terminal behavior.

use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
};

use festerm_core::{
    Attributes, CellWidth, Color, Dimensions, MouseTrackingMode, Terminal, TerminalModes,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fixture {
    pub name: String,
    pub dimensions: Dimensions,
    pub input: Vec<u8>,
    pub resize: Option<Dimensions>,
    pub expected_grid: Vec<String>,
    pub expected_cells: Vec<CellExpectation>,
    pub expected_cursor: (usize, usize),
    pub expected_modes: Option<ModeExpectation>,
    pub expected_dirty_rows: Option<Vec<usize>>,
    pub expected_replies: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CellExpectation {
    column: usize,
    row: usize,
    text: String,
    width: Option<CellWidth>,
    foreground: Color,
    background: Color,
    attributes: Attributes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModeExpectation {
    auto_wrap: bool,
    origin_mode: bool,
    alternate_screen: bool,
    cursor_visible: bool,
    application_cursor: bool,
    application_keypad: bool,
    bracketed_paste: bool,
    focus_reporting: bool,
    mouse_tracking: MouseTrackingMode,
    sgr_mouse: bool,
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
    // Constructor dirtiness is not part of an input scenario. Fixtures that
    // assert `dirty` observe only the bytes and optional resize below.
    terminal.take_dirty_rows();
    terminal.ingest(&fixture.input);
    if let Some(dimensions) = fixture.resize {
        terminal
            .resize(dimensions)
            .map_err(|error| FixtureAssertionError {
                message: format!(
                    "golden fixture `{}` could not resize the terminal: {error}",
                    fixture.name
                ),
            })?;
    }

    let actual_dimensions = fixture.resize.unwrap_or(fixture.dimensions);
    let actual_grid = (0..actual_dimensions.rows())
        .map(|row| {
            terminal
                .row_text(row)
                .expect("row is within fixture dimensions")
        })
        .collect::<Vec<_>>();
    let actual_cursor = (terminal.cursor().column(), terminal.cursor().row());
    let actual_modes = terminal.modes();
    let actual_dirty_rows = fixture
        .expected_dirty_rows
        .as_ref()
        .map(|_| terminal.take_dirty_rows());
    let actual_replies = terminal.drain_replies();
    let cell_mismatches = fixture
        .expected_cells
        .iter()
        .filter_map(|expected| {
            let actual = terminal.cell(expected.column, expected.row)?;
            (actual.text() != expected.text
                || expected.width.is_some_and(|width| actual.width() != width)
                || actual.foreground() != expected.foreground
                || actual.background() != expected.background
                || actual.attributes() != expected.attributes)
                .then(|| {
                    format!(
                        "({}, {}): expected {:?}/{:?}/{:?}/{:?}/{:?}, actual {:?}/{:?}/{:?}/{:?}/{:?}",
                        expected.column,
                        expected.row,
                        expected.text,
                        expected.width,
                        expected.foreground,
                        expected.background,
                        expected.attributes,
                        actual.text(),
                        actual.width(),
                        actual.foreground(),
                        actual.background(),
                        actual.attributes()
                    )
                })
        })
        .collect::<Vec<_>>();
    let modes_match = fixture
        .expected_modes
        .as_ref()
        .is_none_or(|expected| expected.matches(actual_modes));

    if actual_grid != fixture.expected_grid
        || actual_cursor != fixture.expected_cursor
        || !cell_mismatches.is_empty()
        || !modes_match
        || actual_dirty_rows.as_ref() != fixture.expected_dirty_rows.as_ref()
        || actual_replies != fixture.expected_replies
    {
        return Err(FixtureAssertionError {
            message: format!(
                "golden fixture `{}` failed\n\
             expected grid:\n{}\n\
             actual grid:\n{}\n\
             expected cursor: {:?}\n\
             actual cursor: {:?}\n\
             expected cell attributes: {}\n\
             actual cell mismatches: {}\n\
             expected modes: {}\n\
             actual modes: {}\n\
             expected dirty rows: {}\n\
             actual dirty rows: {}\n\
             expected replies: {}\n\
             actual replies: {}",
                fixture.name,
                render_grid(&fixture.expected_grid),
                render_grid(&actual_grid),
                fixture.expected_cursor,
                actual_cursor,
                render_cells(&fixture.expected_cells),
                render_items(&cell_mismatches),
                fixture
                    .expected_modes
                    .as_ref()
                    .map_or_else(|| "(not asserted)".to_owned(), |modes| modes.to_string()),
                render_modes(actual_modes),
                fixture
                    .expected_dirty_rows
                    .as_ref()
                    .map_or_else(|| "(not asserted)".to_owned(), |rows| format!("{rows:?}")),
                actual_dirty_rows
                    .as_ref()
                    .map_or_else(|| "(not asserted)".to_owned(), |rows| format!("{rows:?}")),
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
    let mut resize = None;
    let mut expected_grid = Vec::new();
    let mut expected_cells = Vec::new();
    let mut expected_cursor = None;
    let mut expected_modes = None;
    let mut expected_dirty_rows = None;
    let mut expected_replies = None;
    let mut in_grid = false;
    let mut in_cells = false;

    for (index, raw_line) in source.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line == "grid:" {
            in_grid = true;
            in_cells = false;
            continue;
        }
        if in_grid && line.starts_with('-') {
            expected_grid.push(parse_quoted(path, line_number, line[1..].trim())?);
            continue;
        }
        if line == "cells:" {
            in_cells = true;
            in_grid = false;
            continue;
        }
        if in_cells && line.starts_with('-') {
            expected_cells.push(parse_cell_expectation(path, line_number, line[1..].trim())?);
            continue;
        }
        in_grid = false;
        in_cells = false;

        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| FixtureError::parse(path, line_number, "expected `key: value`"))?;
        match key.trim() {
            "name" => name = Some(value.trim().to_owned()),
            "size" => dimensions = Some(parse_dimensions(path, line_number, value.trim())?),
            "input" => input = Some(parse_quoted_bytes(path, line_number, value.trim())?),
            "resize" => resize = Some(parse_dimensions(path, line_number, value.trim())?),
            "cursor" => expected_cursor = Some(parse_cursor(path, line_number, value.trim())?),
            "modes" => expected_modes = Some(parse_modes(path, line_number, value.trim())?),
            "dirty" => {
                expected_dirty_rows = Some(parse_dirty_rows(path, line_number, value.trim())?)
            }
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
        resize,
        expected_grid,
        expected_cells,
        expected_cursor,
        expected_modes,
        expected_dirty_rows,
        expected_replies,
    };

    let expected_dimensions = fixture.resize.unwrap_or(fixture.dimensions);
    if fixture.expected_grid.len() != expected_dimensions.rows()
        || fixture
            .expected_grid
            .iter()
            .any(|row| row.chars().count() != expected_dimensions.columns())
    {
        return Err(FixtureError::parse(
            path,
            0,
            "grid rows must exactly match the declared dimensions",
        ));
    }
    if fixture.expected_cursor.0 >= expected_dimensions.columns()
        || fixture.expected_cursor.1 >= expected_dimensions.rows()
    {
        return Err(FixtureError::parse(
            path,
            0,
            "cursor must lie within the declared dimensions",
        ));
    }
    for expected in &fixture.expected_cells {
        if expected.column >= expected_dimensions.columns()
            || expected.row >= expected_dimensions.rows()
        {
            return Err(FixtureError::parse(
                path,
                0,
                "cell assertion must lie within the declared dimensions",
            ));
        }
    }
    if let Some(dirty_rows) = &fixture.expected_dirty_rows {
        if dirty_rows
            .iter()
            .any(|row| *row >= expected_dimensions.rows())
        {
            return Err(FixtureError::parse(
                path,
                0,
                "dirty row assertion must lie within the declared dimensions",
            ));
        }
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

fn parse_cell_expectation(
    path: &Path,
    line: usize,
    value: &str,
) -> Result<CellExpectation, FixtureError> {
    let value = parse_quoted(path, line, value)?;
    let mut parts = value.split('|');
    let coordinates = parts
        .next()
        .ok_or_else(|| FixtureError::parse(path, line, "cell assertion is missing coordinates"))?;
    let (column, row) = parse_cursor(path, line, coordinates)?;
    let text = parts
        .next()
        .ok_or_else(|| FixtureError::parse(path, line, "cell assertion is missing text"))?
        .to_owned();
    let foreground = parse_color(
        path,
        line,
        parts.next().ok_or_else(|| {
            FixtureError::parse(path, line, "cell assertion is missing foreground")
        })?,
    )?;
    let background = parse_color(
        path,
        line,
        parts.next().ok_or_else(|| {
            FixtureError::parse(path, line, "cell assertion is missing background")
        })?,
    )?;
    let attributes = parse_attributes(
        path,
        line,
        parts.next().ok_or_else(|| {
            FixtureError::parse(path, line, "cell assertion is missing attributes")
        })?,
    )?;
    let width = parts
        .next()
        .map(|width| parse_cell_width(path, line, width))
        .transpose()?;
    if parts.next().is_some() {
        return Err(FixtureError::parse(
            path,
            line,
            "cell assertion has too many `|`-separated fields",
        ));
    }
    Ok(CellExpectation {
        column,
        row,
        text,
        width,
        foreground,
        background,
        attributes,
    })
}

fn parse_cell_width(path: &Path, line: usize, value: &str) -> Result<CellWidth, FixtureError> {
    match value.trim() {
        "single" => Ok(CellWidth::Single),
        "double" => Ok(CellWidth::Double),
        "continuation" => Ok(CellWidth::Continuation),
        _ => Err(FixtureError::parse(
            path,
            line,
            "cell width must be `single`, `double`, or `continuation`",
        )),
    }
}

fn parse_color(path: &Path, line: usize, value: &str) -> Result<Color, FixtureError> {
    let value = value.trim();
    if value == "default" {
        return Ok(Color::Default);
    }
    if let Some(value) = value.strip_prefix("indexed:") {
        return value.parse().map(Color::Indexed).map_err(|error| {
            FixtureError::parse(path, line, format!("invalid indexed color: {error}"))
        });
    }
    if let Some(value) = value.strip_prefix("rgb:") {
        let components = value
            .split(',')
            .map(|component| {
                component.trim().parse::<u8>().map_err(|error| {
                    FixtureError::parse(path, line, format!("invalid RGB component: {error}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let [red, green, blue] = components.as_slice() {
            return Ok(Color::Rgb {
                red: *red,
                green: *green,
                blue: *blue,
            });
        }
    }
    Err(FixtureError::parse(
        path,
        line,
        "color must be `default`, `indexed:<0-255>`, or `rgb:<r>,<g>,<b>`",
    ))
}

fn parse_attributes(path: &Path, line: usize, value: &str) -> Result<Attributes, FixtureError> {
    let value = value.trim();
    if value == "none" {
        return Ok(Attributes::NONE);
    }

    value
        .split(',')
        .try_fold(Attributes::NONE, |attributes, name| {
            let attribute = match name.trim() {
                "bold" => Attributes::BOLD,
                "faint" => Attributes::FAINT,
                "italic" => Attributes::ITALIC,
                "underline" => Attributes::UNDERLINE,
                "double-underline" => Attributes::DOUBLE_UNDERLINE,
                "slow-blink" => Attributes::SLOW_BLINK,
                "rapid-blink" => Attributes::RAPID_BLINK,
                "inverse" => Attributes::INVERSE,
                "concealed" => Attributes::CONCEALED,
                "strikethrough" => Attributes::STRIKETHROUGH,
                other => {
                    return Err(FixtureError::parse(
                        path,
                        line,
                        format!("unsupported attribute `{other}`"),
                    ));
                }
            };
            Ok(Attributes::from_bits(attributes.bits() | attribute.bits()))
        })
}

fn parse_modes(path: &Path, line: usize, value: &str) -> Result<ModeExpectation, FixtureError> {
    let mut expected = ModeExpectation {
        auto_wrap: true,
        origin_mode: false,
        alternate_screen: false,
        cursor_visible: true,
        application_cursor: false,
        application_keypad: false,
        bracketed_paste: false,
        focus_reporting: false,
        mouse_tracking: MouseTrackingMode::None,
        sgr_mouse: false,
    };
    for assignment in parse_quoted(path, line, value)?.split(',') {
        let (name, value) = assignment.split_once('=').ok_or_else(|| {
            FixtureError::parse(path, line, "mode must use `name=true` or `name=false`")
        })?;
        match name.trim() {
            "mouse_tracking" => {
                expected.mouse_tracking = parse_mouse_tracking(path, line, value.trim())?
            }
            "auto_wrap" => expected.auto_wrap = parse_mode_bool(path, line, value)?,
            "origin_mode" => expected.origin_mode = parse_mode_bool(path, line, value)?,
            "alternate_screen" => expected.alternate_screen = parse_mode_bool(path, line, value)?,
            "cursor_visible" => expected.cursor_visible = parse_mode_bool(path, line, value)?,
            "application_cursor" => {
                expected.application_cursor = parse_mode_bool(path, line, value)?
            }
            "application_keypad" => {
                expected.application_keypad = parse_mode_bool(path, line, value)?
            }
            "bracketed_paste" => expected.bracketed_paste = parse_mode_bool(path, line, value)?,
            "focus_reporting" => expected.focus_reporting = parse_mode_bool(path, line, value)?,
            "sgr_mouse" => expected.sgr_mouse = parse_mode_bool(path, line, value)?,
            other => {
                return Err(FixtureError::parse(
                    path,
                    line,
                    format!("unsupported mode `{other}`"),
                ));
            }
        }
    }
    Ok(expected)
}

fn parse_mode_bool(path: &Path, line: usize, value: &str) -> Result<bool, FixtureError> {
    value.trim().parse::<bool>().map_err(|error| {
        FixtureError::parse(path, line, format!("invalid mode value `{value}`: {error}"))
    })
}

fn parse_mouse_tracking(
    path: &Path,
    line: usize,
    value: &str,
) -> Result<MouseTrackingMode, FixtureError> {
    match value {
        "none" => Ok(MouseTrackingMode::None),
        "x10" => Ok(MouseTrackingMode::X10),
        "button-event" => Ok(MouseTrackingMode::ButtonEvent),
        "button-motion" => Ok(MouseTrackingMode::ButtonMotion),
        "any-motion" => Ok(MouseTrackingMode::AnyMotion),
        _ => Err(FixtureError::parse(
            path,
            line,
            "mouse tracking must be `none`, `x10`, `button-event`, `button-motion`, or `any-motion`",
        )),
    }
}

fn parse_dirty_rows(path: &Path, line: usize, value: &str) -> Result<Vec<usize>, FixtureError> {
    let value = parse_quoted(path, line, value)?;
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|row| parse_usize(path, line, row, "dirty row"))
        .collect()
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

impl ModeExpectation {
    fn matches(&self, modes: TerminalModes) -> bool {
        self.auto_wrap == modes.auto_wrap()
            && self.origin_mode == modes.origin_mode()
            && self.alternate_screen == modes.alternate_screen()
            && self.cursor_visible == modes.cursor_visible()
            && self.application_cursor == modes.application_cursor()
            && self.application_keypad == modes.application_keypad()
            && self.bracketed_paste == modes.bracketed_paste()
            && self.focus_reporting == modes.focus_reporting()
            && self.mouse_tracking == modes.mouse_tracking()
            && self.sgr_mouse == modes.sgr_mouse()
    }
}

impl fmt::Display for ModeExpectation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "auto_wrap={}, origin_mode={}, alternate_screen={}, cursor_visible={}, application_cursor={}, application_keypad={}, bracketed_paste={}, focus_reporting={}, mouse_tracking={:?}, sgr_mouse={}",
            self.auto_wrap,
            self.origin_mode,
            self.alternate_screen,
            self.cursor_visible,
            self.application_cursor,
            self.application_keypad,
            self.bracketed_paste,
            self.focus_reporting,
            self.mouse_tracking,
            self.sgr_mouse
        )
    }
}

fn render_grid(grid: &[String]) -> String {
    grid.iter()
        .enumerate()
        .map(|(row, content)| format!("  {row:>3} |{content}|"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_cells(cells: &[CellExpectation]) -> String {
    if cells.is_empty() {
        return "(none)".to_owned();
    }
    cells
        .iter()
        .map(|cell| {
            format!(
                "({}, {}): {:?}/{:?}/{:?}/{:?}/{:?}",
                cell.column,
                cell.row,
                cell.text,
                cell.width,
                cell.foreground,
                cell.background,
                cell.attributes
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_items(items: &[String]) -> String {
    if items.is_empty() {
        return "(none)".to_owned();
    }
    items.join(", ")
}

fn render_modes(modes: TerminalModes) -> String {
    format!(
        "auto_wrap={}, origin_mode={}, alternate_screen={}, cursor_visible={}, application_cursor={}, application_keypad={}, bracketed_paste={}, focus_reporting={}, mouse_tracking={:?}, sgr_mouse={}",
        modes.auto_wrap(),
        modes.origin_mode(),
        modes.alternate_screen(),
        modes.cursor_visible(),
        modes.application_cursor(),
        modes.application_keypad(),
        modes.bracketed_paste(),
        modes.focus_reporting(),
        modes.mouse_tracking(),
        modes.sgr_mouse()
    )
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
