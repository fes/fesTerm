use compact_str::CompactString;
use serde::{
    de::{Error as _, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use smallvec::SmallVec;
use std::{fmt, marker::PhantomData};

use crate::{
    cell::{blank_cell, Attributes, Cell, CellWidth, FAINT_OPACITY},
    terminal::{TerminalError, MAX_RECORDING_CHECKPOINT_BYTES},
    unicode::MAX_GRAPHEME_BYTES,
    Color, ColorScheme, Dimensions, Rgb,
};

/// A visible grid and its redraw state.
///
/// Rows are stored in a ring buffer: `top` is the physical row index that
/// currently holds logical row 0. Scrolling the whole screen up or down by
/// `n` rows (by far the hottest path under sustained output - every
/// newline scrolls by one row once the cursor reaches the last row) is
/// therefore an O(n) rotation of `top` plus clearing the rows it reveals,
/// instead of an O(rows*columns) clone of the entire grid on every line.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Screen {
    dimensions: Dimensions,
    /// Physical row index holding logical row 0.
    top: usize,
    cells: Vec<Cell>,
    occupied_cells: Vec<bool>,
    dirty_rows: Vec<bool>,
    soft_wrapped_rows: Vec<bool>,
    occupied_columns: Vec<usize>,
}

const RECORDING_CELL_LIMIT: usize =
    (MAX_RECORDING_CHECKPOINT_BYTES - 4096) / (std::mem::size_of::<Cell>() + 32);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RecordingCell {
    #[serde(deserialize_with = "deserialize_recording_text")]
    text: CompactString,
    width: CellWidth,
    foreground: Color,
    background: Color,
    attributes: Attributes,
}

impl RecordingCell {
    fn from_cell(cell: &Cell, scheme: &ColorScheme, compare_text: bool) -> Self {
        let hidden =
            recording_text_is_hidden(cell.foreground, cell.background, cell.attributes, scheme);
        Self {
            text: if hidden && !compare_text && cell.width != CellWidth::Continuation {
                CompactString::const_new(" ")
            } else {
                cell.text.clone()
            },
            width: cell.width,
            foreground: cell.foreground,
            background: cell.background,
            attributes: cell.attributes,
        }
    }

    fn into_cell(self) -> Cell {
        let text_capacity_bytes = Cell::text_capacity(&self.text);
        Cell {
            text: self.text,
            text_capacity_bytes,
            width: self.width,
            foreground: self.foreground,
            background: self.background,
            attributes: self.attributes,
            hyperlink: None,
        }
    }
}

pub(crate) fn recording_text_is_hidden(
    foreground: Color,
    background: Color,
    attributes: Attributes,
    scheme: &ColorScheme,
) -> bool {
    if attributes.contains(Attributes::CONCEALED) {
        return true;
    }
    let mut foreground = scheme.resolve(foreground, scheme.foreground());
    let mut background = scheme.resolve(background, scheme.background());
    if attributes.contains(Attributes::INVERSE) {
        std::mem::swap(&mut foreground, &mut background);
    }
    foreground == background
        || (attributes.contains(Attributes::FAINT)
            && faint_can_match_background(foreground, background))
}

fn faint_can_match_background(foreground: Rgb, background: Rgb) -> bool {
    // Both gamma and sRGB framebuffer blending can round faint ink into its background.
    let foreground = [foreground.red, foreground.green, foreground.blue];
    let background = [background.red, background.green, background.blue];
    let opacity = f64::from((255.0 * FAINT_OPACITY + 0.5) as u8) / 255.0;
    let mut gamma_matches = true;
    let mut linear_matches = true;
    for (foreground, background) in foreground.into_iter().zip(background) {
        let premultiplied = f64::from((f32::from(foreground) * FAINT_OPACITY + 0.5) as u8);
        let background = f64::from(background);
        let gamma = premultiplied + background * (1.0 - opacity);
        gamma_matches &= (gamma.round() - background).abs() <= 1.0;

        let to_linear = |value: f64| {
            let value = value / 255.0;
            if value < 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        let linear = to_linear(premultiplied) + to_linear(background) * (1.0 - opacity);
        let gamma = if linear < 0.0031308 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        linear_matches &= ((gamma * 255.0).round() - background).abs() <= 1.0;
    }
    gamma_matches || linear_matches
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct RecordingScreen {
    dimensions: Dimensions,
    top: usize,
    #[serde(deserialize_with = "deserialize_recording_vec")]
    cells: Vec<RecordingCell>,
    #[serde(deserialize_with = "deserialize_recording_vec")]
    occupied_cells: Vec<bool>,
    #[serde(deserialize_with = "deserialize_recording_vec")]
    soft_wrapped_rows: Vec<bool>,
    #[serde(deserialize_with = "deserialize_recording_vec")]
    occupied_columns: Vec<usize>,
}

pub(crate) fn deserialize_recording_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct BoundedVec<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for BoundedVec<T> {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bounded recording sequence")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Vec<T>, A::Error> {
            let capacity = sequence.size_hint().unwrap_or(0);
            if capacity > RECORDING_CELL_LIMIT {
                return Err(A::Error::custom("recording sequence exceeds its limit"));
            }
            let mut values = Vec::with_capacity(capacity);
            while let Some(value) = sequence.next_element()? {
                if values.len() == RECORDING_CELL_LIMIT {
                    return Err(A::Error::custom("recording sequence exceeds its limit"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(BoundedVec(PhantomData))
}

fn deserialize_recording_text<'de, D>(deserializer: D) -> Result<CompactString, D::Error>
where
    D: Deserializer<'de>,
{
    struct BoundedText;

    impl Visitor<'_> for BoundedText {
        type Value = CompactString;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("bounded terminal cell text")
        }

        fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
            if text.len() > MAX_GRAPHEME_BYTES {
                return Err(E::custom("recording cell text exceeds its limit"));
            }
            Ok(CompactString::new(text))
        }
    }

    deserializer.deserialize_str(BoundedText)
}

impl RecordingScreen {
    pub(crate) fn allocation_bound(&self) -> Option<usize> {
        let cells = self
            .cells
            .len()
            .checked_mul(std::mem::size_of::<Cell>() + 32)?;
        let rows = self
            .dimensions
            .rows()
            .checked_mul(std::mem::size_of::<usize>() + 3)?;
        self.cells
            .iter()
            .try_fold(cells.checked_add(rows)?, |bytes, cell| {
                bytes.checked_add(cell.text.capacity())
            })
    }

    pub(crate) fn into_screen(self) -> Result<Screen, String> {
        let dimensions = Dimensions::new(self.dimensions.columns(), self.dimensions.rows())
            .map_err(|error| format!("recording screen has invalid dimensions: {error}"))?;
        if dimensions != self.dimensions
            || self.cells.len() != dimensions.cell_count()
            || self.occupied_cells.len() != dimensions.cell_count()
            || self.soft_wrapped_rows.len() != dimensions.rows()
            || self.occupied_columns.len() != dimensions.rows()
        {
            return Err("recording screen dimensions and stored lengths do not match".to_owned());
        }
        let screen = Screen {
            dimensions,
            top: self.top,
            cells: self
                .cells
                .into_iter()
                .map(RecordingCell::into_cell)
                .collect(),
            occupied_cells: self.occupied_cells,
            dirty_rows: vec![true; dimensions.rows()],
            soft_wrapped_rows: self.soft_wrapped_rows,
            occupied_columns: self.occupied_columns,
        };
        screen.validate_recovery_state()?;
        Ok(screen)
    }
}

impl Eq for Screen {}

impl PartialEq for Screen {
    /// Compares logical content (as seen through row 0..rows), independent
    /// of the ring buffer's internal `top` rotation.
    fn eq(&self, other: &Self) -> bool {
        if self.dimensions != other.dimensions {
            return false;
        }
        (0..self.dimensions.rows()).all(|row| {
            let columns = self.dimensions.columns();
            let self_start = self.physical_row_start(row);
            let other_start = other.physical_row_start(row);
            self.cells[self_start..self_start + columns]
                == other.cells[other_start..other_start + columns]
                && self.occupied_cells[self_start..self_start + columns]
                    == other.occupied_cells[other_start..other_start + columns]
                && self.dirty_rows[row] == other.dirty_rows[row]
                && self.soft_wrapped_rows[self.physical_row(row)]
                    == other.soft_wrapped_rows[other.physical_row(row)]
                && self.occupied_columns[self.physical_row(row)]
                    == other.occupied_columns[other.physical_row(row)]
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ScreenRow {
    pub(crate) cells: Vec<Cell>,
    pub(crate) soft_wrapped: bool,
}

/// The columns a row-shifting operation is confined to.
///
/// Before DECSLRM has been used this is always the whole row, which is both
/// the overwhelmingly common case and the only one that can take the ring's
/// rotation fast path, so the span is carried explicitly rather than read
/// from the screen: it makes the full-width case checkable in one place.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ColumnSpan {
    pub(crate) left: usize,
    pub(crate) right: usize,
}

/// An absolute, already-clipped rectangle of the screen.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Rectangle {
    pub(crate) top: usize,
    pub(crate) left: usize,
    pub(crate) bottom: usize,
    pub(crate) right: usize,
}

impl ColumnSpan {
    pub(crate) fn new(left: usize, right: usize) -> Self {
        Self { left, right }
    }

    pub(crate) fn full(columns: usize) -> Self {
        Self {
            left: 0,
            right: columns - 1,
        }
    }

    pub(crate) fn width(&self) -> usize {
        self.right + 1 - self.left
    }

    fn is_full(&self, columns: usize) -> bool {
        self.left == 0 && self.right + 1 == columns
    }
}

impl Screen {
    pub fn new(dimensions: Dimensions) -> Result<Self, TerminalError> {
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(dimensions.cell_count())
            .map_err(|error| TerminalError::allocation("screen cells", error))?;
        cells.resize(dimensions.cell_count(), blank_cell());
        let occupied_cells = vec![false; dimensions.cell_count()];

        let mut dirty_rows = Vec::new();
        dirty_rows
            .try_reserve_exact(dimensions.rows())
            .map_err(|error| TerminalError::allocation("screen dirty rows", error))?;
        dirty_rows.resize(dimensions.rows(), true);

        let mut soft_wrapped_rows = Vec::new();
        soft_wrapped_rows
            .try_reserve_exact(dimensions.rows())
            .map_err(|error| TerminalError::allocation("screen wrap metadata", error))?;
        soft_wrapped_rows.resize(dimensions.rows(), false);
        let occupied_columns = vec![0; dimensions.rows()];

        Ok(Self {
            dimensions,
            top: 0,
            cells,
            occupied_cells,
            dirty_rows,
            soft_wrapped_rows,
            occupied_columns,
        })
    }

    pub const fn dimensions(&self) -> Dimensions {
        self.dimensions
    }

    pub(crate) fn recording_allocation_bound(&self) -> Option<usize> {
        let cells = self
            .cells
            .len()
            .checked_mul(std::mem::size_of::<Cell>() + 32)?;
        let rows = self
            .dimensions
            .rows()
            .checked_mul(std::mem::size_of::<usize>() + 3)?;
        self.cells
            .iter()
            .try_fold(cells.checked_add(rows)?, |bytes, cell| {
                bytes.checked_add(cell.text_owned_charge())
            })
    }

    pub(crate) fn recording_copy(
        &self,
        scheme: &ColorScheme,
        compare_ambiguous: bool,
    ) -> RecordingScreen {
        RecordingScreen {
            dimensions: self.dimensions,
            top: self.top,
            cells: self
                .cells
                .iter()
                .map(|cell| {
                    let compare_text =
                        compare_ambiguous && !cell.attributes.contains(Attributes::CONCEALED);
                    RecordingCell::from_cell(cell, scheme, compare_text)
                })
                .collect(),
            occupied_cells: self.occupied_cells.clone(),
            soft_wrapped_rows: self.soft_wrapped_rows.clone(),
            occupied_columns: self.occupied_columns.clone(),
        }
    }

    /// Maps a logical row (0 = the top visible row) to its physical row
    /// index in the ring buffer.
    fn physical_row(&self, logical_row: usize) -> usize {
        let rows = self.dimensions.rows();
        let row = self.top + logical_row;
        // `top < rows` and callers stay in range, so one conditional subtract
        // replaces the division on this per-character path.
        if logical_row < rows {
            if row >= rows {
                row - rows
            } else {
                row
            }
        } else {
            row % rows
        }
    }

    /// The physical cell-array start offset for a logical row.
    fn physical_row_start(&self, logical_row: usize) -> usize {
        self.physical_row(logical_row) * self.dimensions.columns()
    }

    pub fn cell(&self, column: usize, row: usize) -> Option<Cell> {
        self.cells.get(self.cell_index(column, row)?).cloned()
    }

    /// Borrows a cell without cloning the grid or its cell content.
    ///
    /// This is intended for renderer-facing read-only views. Mutation remains
    /// exclusively owned by the terminal state machine.
    pub fn cell_ref(&self, column: usize, row: usize) -> Option<&Cell> {
        self.cells.get(self.cell_index(column, row)?)
    }

    pub fn row_text(&self, row: usize) -> Option<String> {
        if row >= self.dimensions.rows() {
            return None;
        }

        let start = self.physical_row_start(row);
        let end = start + self.dimensions.columns();
        Some(self.cells[start..end].iter().map(Cell::character).collect())
    }

    /// Whether this physical row continues into the next row without a hard
    /// line break.
    pub fn row_soft_wrapped(&self, row: usize) -> Option<bool> {
        (row < self.dimensions.rows()).then(|| self.soft_wrapped_rows[self.physical_row(row)])
    }

    /// Occupied extent, including printed spaces but excluding untouched
    /// trailing cells such as the gap before a width-two character wraps.
    pub fn row_occupied_columns(&self, row: usize) -> Option<usize> {
        (row < self.dimensions.rows()).then(|| self.occupied_columns[self.physical_row(row)])
    }

    pub fn is_row_dirty(&self, row: usize) -> Option<bool> {
        self.dirty_rows.get(row).copied()
    }

    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        self.dirty_rows
            .iter_mut()
            .enumerate()
            .filter_map(|(row, is_dirty)| {
                if *is_dirty {
                    *is_dirty = false;
                    Some(row)
                } else {
                    None
                }
            })
            .collect()
    }

    pub(crate) fn mark_all_dirty(&mut self) {
        self.dirty_rows.fill(true);
    }

    /// One past the last row with any occupied content, or `0` if the
    /// screen is entirely blank. Used to trim wholly-blank trailing rows
    /// before folding the screen into a [`Scrollback`](crate::history) for
    /// reflow, so they don't become phantom empty logical lines.
    pub(crate) fn occupied_row_count(&self) -> usize {
        (0..self.dimensions.rows())
            .rposition(|row| self.occupied_columns[self.physical_row(row)] > 0)
            .map_or(0, |row| row + 1)
    }

    pub(crate) fn occupied_row_cells(&self, row: usize) -> &[Cell] {
        let start = self.physical_row_start(row);
        &self.cells[start..start + self.occupied_columns[self.physical_row(row)]]
    }

    /// Extracts every row as content trimmed to its occupied extent (no
    /// trailing padding), paired with whether it soft-wraps into the next
    /// row. Mirrors the extraction `scroll_up` performs when a row leaves
    /// the screen for retained history, so this screen's current content
    /// can be folded into a [`Scrollback`](crate::history) for a unified
    /// reflow across resize.
    pub(crate) fn to_rows(&self) -> Vec<ScreenRow> {
        (0..self.dimensions.rows())
            .map(|row| {
                let start = self.physical_row_start(row);
                let physical_row = self.physical_row(row);
                ScreenRow {
                    cells: self.cells[start..start + self.occupied_columns[physical_row]].to_vec(),
                    soft_wrapped: self.soft_wrapped_rows[physical_row],
                }
            })
            .collect()
    }

    /// Builds a screen of `dimensions` from previously reflowed rows (for
    /// example, the tail of a [`Scrollback::split_off_tail`] call), one row
    /// per entry from the top down. Rows are assumed to already fit within
    /// `dimensions.columns()`; any shorter row is padded with blank cells
    /// and any surplus rows beyond `dimensions.rows()` are ignored. Every
    /// row is marked dirty for the next redraw.
    pub(crate) fn from_rows(
        dimensions: Dimensions,
        rows: Vec<ScreenRow>,
    ) -> Result<Self, TerminalError> {
        let mut screen = Self::new(dimensions)?;
        let columns = dimensions.columns();
        for (row, screen_row) in rows.into_iter().take(dimensions.rows()).enumerate() {
            let start = row * columns;
            let len = screen_row.cells.len().min(columns);
            screen.cells[start..start + len].clone_from_slice(&screen_row.cells[..len]);
            screen.occupied_cells[start..start + len].fill(true);
            screen.soft_wrapped_rows[row] = screen_row.soft_wrapped;
            screen.occupied_columns[row] = len;
        }
        screen.repair_wide_cells();
        screen.mark_all_dirty();
        Ok(screen)
    }

    pub(crate) fn resized(&self, dimensions: Dimensions) -> Result<Self, TerminalError> {
        let mut resized = Self::new(dimensions)?;
        let preserved_rows = self.dimensions.rows().min(dimensions.rows());
        let preserved_columns = self.dimensions.columns().min(dimensions.columns());
        for row in 0..preserved_rows {
            let old_start = self.physical_row_start(row);
            let new_start = row * dimensions.columns();
            let old_physical_row = self.physical_row(row);
            resized.cells[new_start..new_start + preserved_columns]
                .clone_from_slice(&self.cells[old_start..old_start + preserved_columns]);
            resized.occupied_cells[new_start..new_start + preserved_columns]
                .copy_from_slice(&self.occupied_cells[old_start..old_start + preserved_columns]);
            resized.soft_wrapped_rows[row] = self.soft_wrapped_rows[old_physical_row]
                && preserved_columns == self.dimensions.columns();
            resized.occupied_columns[row] =
                self.occupied_columns[old_physical_row].min(preserved_columns);
        }
        resized.repair_wide_cells();
        Ok(resized)
    }

    pub(crate) fn replace_cell(&mut self, column: usize, row: usize, cell: Cell) {
        let index = self
            .cell_index(column, row)
            .expect("terminal cursor must remain within screen dimensions");
        self.cells[index] = cell;
        self.occupied_cells[index] = true;
        self.recompute_occupied(row);
        self.mark_dirty(row);
    }

    pub(crate) fn replace_cluster(&mut self, column: usize, row: usize, cell: Cell) {
        let index = self
            .cell_index(column, row)
            .expect("terminal cursor must remain within screen dimensions");
        let cluster_columns = cell.width.columns();
        debug_assert!(matches!(cluster_columns, 1 | 2));
        let foreground = cell.foreground;
        let background = cell.background;
        let attributes = cell.attributes;
        let hyperlink = cell.hyperlink.clone();
        // Overwriting a single-width cell with another cannot orphan either
        // half of a neighbouring wide pair, so the repair sweeps are skipped.
        let neighbours_intact =
            cluster_columns == 1 && self.cells[index].width == CellWidth::Single;
        self.cells[index] = cell;
        self.occupied_cells[index] = true;
        if cluster_columns == 2 {
            let continuation = self
                .cell_index(column + 1, row)
                .expect("wide terminal character must fit in the screen");
            let text = CompactString::const_new("");
            let text_capacity_bytes = Cell::text_capacity(&text);
            self.cells[continuation] = Cell {
                text,
                text_capacity_bytes,
                width: CellWidth::Continuation,
                foreground,
                background,
                attributes,
                hyperlink,
            };
            self.occupied_cells[continuation] = true;
        }
        self.mark_dirty(row);
        if !neighbours_intact {
            let fill = blank_cell();
            self.repair_neighborhood(row, column, &fill);
            self.repair_neighborhood(row, column + cluster_columns, &fill);
            self.extend_occupied_or_recompute(row, column, cluster_columns);
        } else {
            // Nothing else changed, so an occupied cell inside the extent
            // leaves it as is: no full-row rescan for TUI mid-row redraws.
            let physical_row = self.physical_row(row);
            let extent = &mut self.occupied_columns[physical_row];
            *extent = (*extent).max(column + 1);
        }
    }

    /// Writes single-width ASCII cells left to right from `column`, exactly
    /// as one [`Self::replace_cluster`] per byte would. The run must fit in
    /// the row. When every target is already single width, no wide pair can
    /// be split, so the cells are written in one pass with one extent update.
    pub(crate) fn write_ascii(
        &mut self,
        column: usize,
        row: usize,
        text: &[u8],
        foreground: crate::Color,
        background: crate::Color,
        attributes: Attributes,
    ) {
        let cell = |byte: u8| {
            let text = CompactString::new(char::from(byte).encode_utf8(&mut [0; 4]));
            let text_capacity_bytes = Cell::text_capacity(&text);
            Cell {
                text,
                text_capacity_bytes,
                width: CellWidth::Single,
                foreground,
                background,
                attributes,
                hyperlink: None,
            }
        };
        let start = self
            .cell_index(column, row)
            .expect("terminal cursor must remain within screen dimensions");
        let end = start + text.len();
        debug_assert!(column + text.len() <= self.dimensions.columns());
        if self.cells[start..end]
            .iter()
            .any(|existing| existing.width != CellWidth::Single)
        {
            for (offset, &byte) in text.iter().enumerate() {
                self.replace_cluster(column + offset, row, cell(byte));
            }
            return;
        }
        for (existing, &byte) in self.cells[start..end].iter_mut().zip(text) {
            let Cell {
                text: existing_text,
                text_capacity_bytes,
                width,
                foreground: existing_foreground,
                background: existing_background,
                attributes: existing_attributes,
                hyperlink,
            } = existing;
            debug_assert_eq!(*width, CellWidth::Single);
            let new_text = CompactString::new(char::from(byte).encode_utf8(&mut [0; 4]));
            *text_capacity_bytes = Cell::text_capacity(&new_text);
            *existing_text = new_text;
            *existing_foreground = foreground;
            *existing_background = background;
            *existing_attributes = attributes;
            *hyperlink = None;
        }
        self.occupied_cells[start..end].fill(true);
        let physical_row = self.physical_row(row);
        let extent = &mut self.occupied_columns[physical_row];
        *extent = (*extent).max(column + text.len());
        self.mark_dirty(row);
    }

    /// Updates `occupied_columns` after writing an occupied cluster at
    /// `[column, column + width)`. Printing strictly at or beyond the row's
    /// previously known occupied extent - the overwhelmingly common case
    /// for left-to-right terminal output - can extend the tracked extent
    /// in `O(1)` instead of [`Self::recompute_occupied`]'s full-row scan,
    /// because nothing occupied exists past the old extent for
    /// [`Self::repair_neighborhood`] to have invalidated (a `Single` cell,
    /// which is what every ordinary printed character is, is also never a
    /// `repair_neighborhood` target: see [`Self::repair_cell`]'s
    /// `CellWidth::Single => false`). Any other write - overwriting
    /// mid-row via cursor movement, for example - falls back to the
    /// authoritative full-row scan, since a wide-character pair broken by
    /// the overwrite could shrink the true occupied extent in a way this
    /// shortcut can't detect. This was the dominant per-character cost
    /// behind fesTerm's raw ingest throughput: profiling a `dir /s`-style
    /// workload showed most ingest time went into rescanning an entire
    /// (already fully occupied) row on every single printed character.
    fn extend_occupied_or_recompute(&mut self, row: usize, column: usize, width: usize) {
        let physical_row = self.physical_row(row);
        if column >= self.occupied_columns[physical_row] {
            self.occupied_columns[physical_row] = column + width;
        } else {
            self.recompute_occupied(row);
        }
    }

    /// Fills a rectangle, optionally leaving protected cells where they are.
    ///
    /// The rectangle operations are defined against the page rather than
    /// against the margins, so this takes absolute bounds that the caller has
    /// already clipped to the screen.
    pub(crate) fn fill_rectangle(
        &mut self,
        rectangle: Rectangle,
        cell: Cell,
        spare_protected: bool,
    ) {
        let occupied = !is_structural_blank(&cell);
        let span = ColumnSpan::new(rectangle.left, rectangle.right);
        for row in rectangle.top..=rectangle.bottom {
            let start = self.physical_row_start(row) + rectangle.left;
            for offset in 0..span.width() {
                if spare_protected
                    && self.cells[start + offset]
                        .attributes
                        .contains(Attributes::PROTECTED)
                {
                    continue;
                }
                self.cells[start + offset] = cell.clone();
                self.occupied_cells[start + offset] = occupied;
            }
            self.repair_span_edges(row, span);
            self.recompute_occupied(row);
        }
        self.mark_dirty_range(rectangle.top, rectangle.bottom);
    }

    /// Copies a rectangle to a new top-left corner, clipped to the screen.
    ///
    /// The source is read out in full before anything is written, because
    /// DECCRA is defined for overlapping rectangles and a cell-by-cell copy
    /// would smear the overlap rather than move it.
    pub(crate) fn copy_rectangle(
        &mut self,
        source: Rectangle,
        destination_top: usize,
        destination_left: usize,
    ) {
        let height = (source.bottom + 1 - source.top).min(self.dimensions.rows() - destination_top);
        let width =
            (source.right + 1 - source.left).min(self.dimensions.columns() - destination_left);
        if height == 0 || width == 0 {
            return;
        }
        let mut copied = Vec::with_capacity(height * width);
        for row in 0..height {
            let start = self.physical_row_start(source.top + row) + source.left;
            for offset in 0..width {
                copied.push((
                    self.cells[start + offset].clone(),
                    self.occupied_cells[start + offset],
                ));
            }
        }
        let span = ColumnSpan::new(destination_left, destination_left + width - 1);
        for row in 0..height {
            let start = self.physical_row_start(destination_top + row) + destination_left;
            for offset in 0..width {
                let (cell, occupied) = copied[row * width + offset].clone();
                self.cells[start + offset] = cell;
                self.occupied_cells[start + offset] = occupied;
            }
            self.repair_span_edges(destination_top + row, span);
            self.recompute_occupied(destination_top + row);
        }
        self.mark_dirty_range(destination_top, destination_top + height - 1);
    }

    pub(crate) fn fill_linear(&mut self, start: usize, end: usize, cell: Cell) {
        self.fill_linear_with(start, end, cell, false);
    }

    /// Fills a span, leaving protected cells where they are.
    ///
    /// This is a selective erase, so the span is walked cell by cell rather
    /// than filled in one go. That is slower, and deliberately confined to
    /// the erases that ask for it: an ordinary `ED` or `EL` keeps the bulk
    /// fill below.
    pub(crate) fn fill_linear_sparing_protected(&mut self, start: usize, end: usize, cell: Cell) {
        self.fill_linear_with(start, end, cell, true);
    }

    fn fill_linear_with(&mut self, start: usize, end: usize, cell: Cell, spare_protected: bool) {
        if start >= end {
            return;
        }
        let columns = self.dimensions.columns();
        let first_row = start / columns;
        let last_row = (end - 1) / columns;
        let first_column = start % columns;
        let last_column = end - last_row * columns;
        let occupied = !is_structural_blank(&cell);
        // Successive logical rows are only contiguous in the cell array while
        // the ring is unrotated, so each row's span is mapped through the ring
        // on its own: a single mapped span would run off the end of the array
        // the moment the fill crosses the wrap point.
        for row in first_row..=last_row {
            let row_start = if row == first_row { first_column } else { 0 };
            let row_end = if row == last_row {
                last_column
            } else {
                columns
            };
            if row_start >= row_end {
                continue;
            }
            let physical_start = self.logical_linear_to_physical(row * columns + row_start);
            let physical_end = physical_start + (row_end - row_start);
            if spare_protected {
                // A spared cell keeps its occupancy along with its content.
                // Taking the fill's would let the row's extent stop short of
                // it, and a scroll trusts the extent both to carry the row
                // into history and to clear the row it recycles.
                for (existing, existing_occupied) in self.cells[physical_start..physical_end]
                    .iter_mut()
                    .zip(&mut self.occupied_cells[physical_start..physical_end])
                {
                    if !existing.attributes.contains(Attributes::PROTECTED) {
                        *existing = cell.clone();
                        *existing_occupied = occupied;
                    }
                }
            } else {
                self.cells[physical_start..physical_end].fill(cell.clone());
                self.occupied_cells[physical_start..physical_end].fill(occupied);
            }
        }
        self.mark_dirty_range(first_row, last_row);
        // A contiguous fill can split a pair only at either range boundary.
        self.repair_neighborhood(first_row, first_column, &cell);
        self.repair_neighborhood(last_row, last_column, &cell);
        for row in first_row..=last_row {
            self.recompute_occupied(row);
        }
    }

    /// Translates a linear index expressed in logical (row 0 = top visible
    /// row) coordinates into the ring buffer's physical cell-array offset.
    /// Only valid for offsets that stay within a single logical row's span,
    /// since a physical row is always contiguous but successive logical
    /// rows may not be (the ring can wrap between them).
    fn logical_linear_to_physical(&self, linear: usize) -> usize {
        let columns = self.dimensions.columns();
        let row = linear / columns;
        let column = linear % columns;
        self.physical_row_start(row) + column
    }

    pub(crate) fn clear_all(&mut self, cell: Cell) {
        let occupied = if is_structural_blank(&cell) {
            0
        } else {
            self.dimensions.columns()
        };
        self.cells.fill(cell);
        self.occupied_cells.fill(occupied != 0);
        self.soft_wrapped_rows.fill(false);
        self.occupied_columns.fill(occupied);
        self.mark_all_dirty();
        // Clearing collapses any pending rotation: logical row 0 is once
        // again physical row 0, keeping the ring buffer's invariant simple
        // for the very common "clear the whole screen" case.
        self.top = 0;
    }

    pub(crate) fn insert_characters(
        &mut self,
        column: usize,
        row: usize,
        count: usize,
        cell: Cell,
        span: ColumnSpan,
    ) {
        let columns = self.dimensions.columns();
        let right = span.right.min(columns - 1);
        if column > right {
            return;
        }
        let count = count.min(right + 1 - column);
        let row_start = self.physical_row_start(row);
        let row_end = row_start + right + 1;
        let start = row_start + column;
        self.move_cells_within_row(start..row_end - count, start + count);
        self.cells[start..start + count].fill(cell.clone());
        self.occupied_cells[start..start + count].fill(!is_structural_blank(&cell));
        self.mark_dirty(row);
        self.repair_neighborhood(row, column, &cell);
        self.repair_neighborhood(row, column + count, &cell);
        self.repair_neighborhood(row, right + 1, &cell);
        self.recompute_occupied(row);
    }

    pub(crate) fn delete_characters(
        &mut self,
        column: usize,
        row: usize,
        count: usize,
        cell: Cell,
        span: ColumnSpan,
    ) {
        let columns = self.dimensions.columns();
        let right = span.right.min(columns - 1);
        if column > right {
            return;
        }
        let count = count.min(right + 1 - column);
        let row_start = self.physical_row_start(row);
        let row_end = row_start + right + 1;
        let start = row_start + column;
        self.move_cells_within_row(start + count..row_end, start);
        self.cells[row_end - count..row_end].fill(cell.clone());
        self.occupied_cells[row_end - count..row_end].fill(!is_structural_blank(&cell));
        self.mark_dirty(row);
        self.repair_neighborhood(row, column, &cell);
        self.repair_neighborhood(row, right + 1 - count, &cell);
        self.repair_neighborhood(row, right + 1, &cell);
        self.recompute_occupied(row);
    }

    pub(crate) fn insert_lines(
        &mut self,
        row: usize,
        bottom: usize,
        count: usize,
        cell: Cell,
        span: ColumnSpan,
    ) {
        let count = count.min(bottom - row + 1);
        if span.is_full(self.dimensions.columns()) {
            for logical in (row..bottom + 1 - count).rev() {
                self.copy_row(logical, logical + count);
            }
            for logical in row..row + count {
                self.fill_row(logical, cell.clone());
            }
        } else {
            for logical in (row..bottom + 1 - count).rev() {
                self.copy_row_span(logical, logical + count, span);
            }
            for logical in row..row + count {
                self.fill_row_span(logical, cell.clone(), span);
            }
        }
        self.mark_dirty_range(row, bottom);
    }

    pub(crate) fn delete_lines(
        &mut self,
        row: usize,
        bottom: usize,
        count: usize,
        cell: Cell,
        span: ColumnSpan,
    ) {
        let count = count.min(bottom - row + 1);
        if span.is_full(self.dimensions.columns()) {
            for logical in row + count..=bottom {
                self.copy_row(logical, logical - count);
            }
            for logical in bottom + 1 - count..=bottom {
                self.fill_row(logical, cell.clone());
            }
        } else {
            for logical in row + count..=bottom {
                self.copy_row_span(logical, logical - count, span);
            }
            for logical in bottom + 1 - count..=bottom {
                self.fill_row_span(logical, cell.clone(), span);
            }
        }
        self.mark_dirty_range(row, bottom);
    }

    pub(crate) fn scroll_up(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        cell: Cell,
        span: ColumnSpan,
        retain_history: bool,
    ) -> SmallVec<[ScreenRow; 1]> {
        let count = count.min(bottom - top + 1);
        if !span.is_full(self.dimensions.columns()) {
            // Only part of each row moves, so nothing leaves the screen as a
            // whole line and there is nothing to hand to scrollback.
            for logical in top + count..=bottom {
                self.copy_row_span(logical, logical - count, span);
            }
            for logical in bottom + 1 - count..=bottom {
                self.fill_row_span(logical, cell.clone(), span);
            }
            self.mark_dirty_range(top, bottom);
            return SmallVec::new();
        }
        let removed = if retain_history {
            // Departing cells move into history before their rows are
            // overwritten; discarded rows can simply be overwritten.
            (top..top + count)
                .map(|row| {
                    let occupied = self.occupied_columns[self.physical_row(row)];
                    self.take_row_into(row, occupied, Vec::with_capacity(occupied))
                })
                .collect()
        } else {
            SmallVec::new()
        };
        if top == 0 && bottom + 1 == self.dimensions.rows() {
            // The common case: the whole screen scrolls. Rotating the ring
            // by `count` avoids moving any surviving row's cells at all -
            // only the `count` newly revealed rows at the bottom need to be
            // cleared, an O(count) operation instead of O(rows*columns).
            let rows = self.dimensions.rows();
            for offset in 0..count {
                self.clear_row(offset, &cell);
            }
            self.top = (self.top + count) % rows;
        } else {
            // A partial scroll region still requires shifting the affected
            // rows physically, since the ring rotation only ever applies to
            // the whole screen. Rows move toward lower indices, so copy
            // forward (destination is never re-read as a source before it's
            // written, since its source index is always still ahead).
            for logical in top + count..=bottom {
                self.copy_row(logical, logical - count);
            }
            for logical in bottom + 1 - count..=bottom {
                self.fill_row(logical, cell.clone());
            }
        }
        self.mark_dirty_range(top, bottom);
        removed
    }

    pub(crate) fn scroll_up_reusing(&mut self, cells: Vec<Cell>, fill: Cell) -> ScreenRow {
        let occupied = self.occupied_columns[self.physical_row(0)];
        let row = self.take_row_into(0, occupied, cells);
        let removed = self.scroll_up(
            0,
            self.dimensions.rows() - 1,
            1,
            fill,
            ColumnSpan::full(self.dimensions.columns()),
            false,
        );
        debug_assert!(removed.is_empty());
        row
    }

    #[inline]
    fn take_row_into(&mut self, row: usize, occupied: usize, mut cells: Vec<Cell>) -> ScreenRow {
        debug_assert!(cells.is_empty() && cells.capacity() >= occupied);
        let start = self.physical_row_start(row);
        for cell in &mut self.cells[start..start + occupied] {
            cells.push(std::mem::replace(cell, blank_cell()));
        }
        ScreenRow {
            cells,
            soft_wrapped: self.soft_wrapped_rows[self.physical_row(row)],
        }
    }

    pub(crate) fn scroll_down(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        cell: Cell,
        span: ColumnSpan,
    ) {
        let count = count.min(bottom - top + 1);
        if span.is_full(self.dimensions.columns()) {
            for logical in (top..bottom + 1 - count).rev() {
                self.copy_row(logical, logical + count);
            }
            for logical in top..top + count {
                self.fill_row(logical, cell.clone());
            }
        } else {
            for logical in (top..bottom + 1 - count).rev() {
                self.copy_row_span(logical, logical + count, span);
            }
            for logical in top..top + count {
                self.fill_row_span(logical, cell.clone(), span);
            }
        }
        self.mark_dirty_range(top, bottom);
    }

    pub(crate) fn mark_soft_wrapped(&mut self, row: usize) {
        // A width-two glyph can pre-wrap before the final column, so the
        // logical continuation flag and occupied extent are independent.
        let physical_row = self.physical_row(row);
        self.soft_wrapped_rows[physical_row] = true;
    }

    /// Records that a row ends a logical line rather than continuing onto
    /// the next one. A line feed proves this even for a row that wrapped
    /// earlier, and reverse wraparound must not climb across such a row.
    pub(crate) fn clear_soft_wrapped(&mut self, row: usize) {
        let physical_row = self.physical_row(row);
        self.soft_wrapped_rows[physical_row] = false;
    }

    fn cell_index(&self, column: usize, row: usize) -> Option<usize> {
        (column < self.dimensions.columns() && row < self.dimensions.rows())
            .then_some(self.physical_row_start(row) + column)
    }

    /// Copies one logical row's cells and metadata onto another logical
    /// row. Used by the line/region-shifting operations that can't be
    /// expressed as a whole-screen ring rotation (insert/delete lines,
    /// scroll within a bounded region, `scroll_down`).
    fn copy_row(&mut self, source_row: usize, destination_row: usize) {
        let columns = self.dimensions.columns();
        let source_start = self.physical_row_start(source_row);
        let destination_start = self.physical_row_start(destination_row);
        if source_start == destination_start {
            return;
        }
        // A physical row is always contiguous in memory even though logical
        // rows can straddle the ring's wrap point, so a plain slice copy
        // (rather than `move_cells`'s cross-region element loop) suffices.
        let (left, right) = if source_start < destination_start {
            let (left, right) = self.cells.split_at_mut(destination_start);
            (
                &mut left[source_start..source_start + columns],
                &mut right[..columns],
            )
        } else {
            let (left, right) = self.cells.split_at_mut(source_start);
            (
                &mut left[destination_start..destination_start + columns],
                &mut right[..columns],
            )
        };
        let (destination_cells, source_cells) = if source_start < destination_start {
            (right, &*left)
        } else {
            (left, &*right)
        };
        destination_cells.clone_from_slice(source_cells);
        self.occupied_cells
            .copy_within(source_start..source_start + columns, destination_start);
        let source_physical = self.physical_row(source_row);
        let destination_physical = self.physical_row(destination_row);
        self.soft_wrapped_rows[destination_physical] = self.soft_wrapped_rows[source_physical];
        self.occupied_columns[destination_physical] = self.occupied_columns[source_physical];
    }

    /// Overwrites an entire logical row with `cell`, matching the fill
    /// behavior of a full-width `fill_linear` call for that row.
    fn fill_row(&mut self, row: usize, cell: Cell) {
        let columns = self.dimensions.columns();
        let start = self.physical_row_start(row);
        let occupied = !is_structural_blank(&cell);
        self.cells[start..start + columns].fill(cell);
        self.occupied_cells[start..start + columns].fill(occupied);
        let physical_row = self.physical_row(row);
        self.soft_wrapped_rows[physical_row] = false;
        self.occupied_columns[physical_row] = row_extent(occupied, columns);
    }

    /// [`Self::fill_row`] for a row whose old content is being discarded.
    ///
    /// Cells past a row's occupied extent are structural blanks already (every
    /// writer records `occupied = !is_structural_blank`), so when the fill is
    /// itself a structural blank only the occupied prefix needs rewriting.
    fn clear_row(&mut self, row: usize, cell: &Cell) {
        let physical_row = self.physical_row(row);
        if !is_structural_blank(cell) {
            self.fill_row(row, cell.clone());
            return;
        }
        let columns = self.dimensions.columns();
        let start = self.physical_row_start(row);
        let extent = self.occupied_columns[physical_row];
        debug_assert!((extent..columns).all(|offset| {
            !self.occupied_cells[start + offset] && is_structural_blank(&self.cells[start + offset])
        }));
        for existing in &mut self.cells[start..start + extent] {
            existing.clone_from(cell);
        }
        self.occupied_cells[start..start + extent].fill(false);
        self.soft_wrapped_rows[physical_row] = false;
        self.occupied_columns[physical_row] = 0;
    }

    /// Copies part of one logical row onto the same columns of another.
    ///
    /// The whole-row `copy_row` moves the line's identity with it - its
    /// soft-wrap flag and occupied extent travel along, because the line
    /// itself moved. Within left/right margins only a window of each row
    /// moves, and the rows either side of that window stay where they are,
    /// so the destination keeps its own wrap flag and its extent has to be
    /// recomputed from the cells rather than copied.
    fn copy_row_span(&mut self, source_row: usize, destination_row: usize, span: ColumnSpan) {
        let source_start = self.physical_row_start(source_row) + span.left;
        let destination_start = self.physical_row_start(destination_row) + span.left;
        if source_start == destination_start {
            return;
        }
        for offset in 0..span.width() {
            self.cells[destination_start + offset] = self.cells[source_start + offset].clone();
            self.occupied_cells[destination_start + offset] =
                self.occupied_cells[source_start + offset];
        }
        self.repair_span_edges(destination_row, span);
        self.recompute_occupied(destination_row);
    }

    /// Overwrites the columns within `span` of one logical row with `cell`.
    fn fill_row_span(&mut self, row: usize, cell: Cell, span: ColumnSpan) {
        let start = self.physical_row_start(row) + span.left;
        let occupied = !is_structural_blank(&cell);
        self.cells[start..start + span.width()].fill(cell);
        self.occupied_cells[start..start + span.width()].fill(occupied);
        self.repair_span_edges(row, span);
        self.recompute_occupied(row);
    }

    /// A wide glyph can straddle a margin, and moving or clearing one side of
    /// it would otherwise leave half a character behind. Both edges are swept
    /// so the surviving half becomes a blank.
    fn repair_span_edges(&mut self, row: usize, span: ColumnSpan) {
        let fill = blank_cell();
        self.repair_neighborhood(row, span.left, &fill);
        self.repair_neighborhood(row, span.right + 1, &fill);
    }

    fn move_cells_within_row(&mut self, source: std::ops::Range<usize>, destination: usize) {
        let length = source.end - source.start;
        if destination > source.start {
            for offset in (0..length).rev() {
                self.cells[destination + offset] = self.cells[source.start + offset].clone();
                self.occupied_cells[destination + offset] =
                    self.occupied_cells[source.start + offset];
            }
        } else {
            for offset in 0..length {
                self.cells[destination + offset] = self.cells[source.start + offset].clone();
                self.occupied_cells[destination + offset] =
                    self.occupied_cells[source.start + offset];
            }
        }
    }

    fn mark_dirty(&mut self, row: usize) {
        self.dirty_rows[row] = true;
    }

    pub(crate) fn mark_dirty_range(&mut self, first: usize, last: usize) {
        self.dirty_rows[first..=last].fill(true);
    }

    fn repair_wide_cells(&mut self) {
        for row in 0..self.dimensions.rows() {
            self.repair_row(row);
        }
    }

    fn repair_row(&mut self, row: usize) {
        let fill = blank_cell();
        for column in 0..self.dimensions.columns() {
            self.repair_cell(row, column, &fill);
        }
        self.recompute_occupied(row);
    }

    fn repair_neighborhood(&mut self, row: usize, boundary: usize, fill: &Cell) {
        let columns = self.dimensions.columns();
        let first = boundary.saturating_sub(1);
        let last = boundary.saturating_add(1).min(columns - 1);
        if first > last {
            return;
        }
        for column in first..=last {
            self.repair_cell(row, column, fill);
        }
    }

    fn repair_cell(&mut self, row: usize, column: usize, fill: &Cell) {
        let columns = self.dimensions.columns();
        let index = self.physical_row_start(row) + column;
        let invalid = match self.cells[index].width {
            CellWidth::Double => {
                column + 1 == columns || self.cells[index + 1].width != CellWidth::Continuation
            }
            CellWidth::Continuation => {
                column == 0 || self.cells[index - 1].width != CellWidth::Double
            }
            CellWidth::Single => false,
        };
        if invalid {
            self.cells[index] = fill.clone();
            self.occupied_cells[index] = !is_structural_blank(fill);
        }
    }

    fn recompute_occupied(&mut self, row: usize) {
        let start = self.physical_row_start(row);
        let physical_row = self.physical_row(row);
        self.occupied_columns[physical_row] = self.occupied_cells
            [start..start + self.dimensions.columns()]
            .iter()
            .rposition(|occupied| *occupied)
            .map_or(0, |column| column + 1);
    }

    pub(crate) fn validate_recovery_state(&self) -> Result<(), String> {
        let expected_dimensions =
            Dimensions::new(self.dimensions.columns(), self.dimensions.rows())
                .map_err(|error| format!("screen stores invalid dimensions: {error}"))?;
        if expected_dimensions != self.dimensions {
            return Err("screen dimensions cache does not match its row/column counts".to_owned());
        }
        if self.top >= self.dimensions.rows() {
            return Err(format!(
                "screen top row {} exceeds the {} visible rows",
                self.top,
                self.dimensions.rows()
            ));
        }
        let expected_cells = self.dimensions.cell_count();
        if self.cells.len() != expected_cells {
            return Err(format!(
                "screen stores {} cells for a {}-cell grid",
                self.cells.len(),
                expected_cells
            ));
        }
        if self.occupied_cells.len() != expected_cells {
            return Err(format!(
                "screen stores {} occupied markers for a {}-cell grid",
                self.occupied_cells.len(),
                expected_cells
            ));
        }
        if self.dirty_rows.len() != self.dimensions.rows() {
            return Err(format!(
                "screen stores {} dirty-row markers for {} rows",
                self.dirty_rows.len(),
                self.dimensions.rows()
            ));
        }
        if self.soft_wrapped_rows.len() != self.dimensions.rows() {
            return Err(format!(
                "screen stores {} soft-wrap markers for {} rows",
                self.soft_wrapped_rows.len(),
                self.dimensions.rows()
            ));
        }
        if self.occupied_columns.len() != self.dimensions.rows() {
            return Err(format!(
                "screen stores {} occupied-column counters for {} rows",
                self.occupied_columns.len(),
                self.dimensions.rows()
            ));
        }
        let columns = self.dimensions.columns();
        for row in 0..self.dimensions.rows() {
            let physical_row = self.physical_row(row);
            let start = self.physical_row_start(row);
            let end = start + columns;
            let occupied_columns = self.occupied_columns[physical_row];
            if occupied_columns > columns {
                return Err(format!(
                    "screen row {} records {} occupied columns in a {}-column grid",
                    row, occupied_columns, columns
                ));
            }
            let expected_occupied = self.occupied_cells[start..end]
                .iter()
                .rposition(|occupied| *occupied)
                .map_or(0, |column| column + 1);
            if occupied_columns != expected_occupied {
                return Err(format!(
                    "screen row {} records {} occupied columns but contains {} occupied cells",
                    row, occupied_columns, expected_occupied
                ));
            }
            for column in 0..columns {
                self.cells[start + column]
                    .validate_recovery_state()
                    .map_err(|error| {
                        format!("screen row {row} column {column} stores invalid cell: {error}")
                    })?;
                match self.cells[start + column].width {
                    CellWidth::Continuation => {
                        if column == 0
                            || !matches!(self.cells[start + column - 1].width, CellWidth::Double)
                        {
                            return Err(format!(
                                "screen row {} column {} contains an orphaned continuation cell",
                                row, column
                            ));
                        }
                    }
                    CellWidth::Double => {
                        if column + 1 >= columns
                            || !matches!(
                                self.cells[start + column + 1].width,
                                CellWidth::Continuation
                            )
                        {
                            return Err(format!(
                                "screen row {} column {} contains an unterminated double-width cell",
                                row, column
                            ));
                        }
                    }
                    CellWidth::Single => {}
                }
            }
        }
        Ok(())
    }

    pub(crate) fn restore_recovery_allocations(&mut self) {
        for cell in &mut self.cells {
            cell.restore_recovery_allocation();
        }
    }
}

fn is_structural_blank(cell: &Cell) -> bool {
    cell.text == " "
        && cell.width == CellWidth::Single
        && cell.foreground == crate::Color::Default
        && cell.background == crate::Color::Default
        && cell.attributes == crate::Attributes::NONE
        && cell.hyperlink.is_none()
}

const fn row_extent(occupied: bool, columns: usize) -> usize {
    if occupied {
        columns
    } else {
        0
    }
}
