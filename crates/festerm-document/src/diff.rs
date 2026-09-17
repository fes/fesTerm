//! The line-oriented comparison Compare renders (ADR 0034 §6).
//!
//! Compare is deliberately line-level rather than word- or semantic-level: it
//! exists so a person can decide which whole version to keep, not so fesTerm
//! can merge for them. Every changed line also carries a `-` or `+` marker, so
//! the comparison is readable without colour (ADR 0034 §8).

use std::ops::Range;

/// Which version a line belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffSide {
    /// The in-memory buffer.
    Mine,
    /// The content the source now holds.
    Source,
}

impl DiffSide {
    /// The pane heading, which names *where* the version is rather than
    /// labelling one of them "theirs".
    pub const fn heading(self, remote: bool) -> &'static str {
        match (self, remote) {
            (Self::Mine, _) => "Your version · unsaved",
            (Self::Source, true) => "On the remote host",
            (Self::Source, false) => "On disk",
        }
    }
}

/// How one line differs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineChange {
    Unchanged,
    Removed,
    Added,
}

impl LineChange {
    /// The leading marker, which is what makes the diff legible in monochrome.
    pub const fn marker(self) -> &'static str {
        match self {
            Self::Unchanged => " ",
            Self::Removed => "-",
            Self::Added => "+",
        }
    }
}

/// One line on one side of the comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLine {
    pub number: usize,
    pub text: String,
    pub change: LineChange,
}

/// One row of the two-pane comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LineComparisonRow {
    /// A line present on one or both sides. A missing side is drawn as an
    /// empty gutter so the two panes stay aligned.
    Pair {
        left: Option<DiffLine>,
        right: Option<DiffLine>,
    },
    /// A run of identical lines that is folded away.
    Collapsed { lines: usize },
}

/// The whole comparison, ready to render.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineComparison {
    rows: Vec<LineComparisonRow>,
    change_count: usize,
    truncated: bool,
}

impl LineComparison {
    /// How many unchanged lines are kept either side of a change before the
    /// rest of the run is folded away.
    const CONTEXT_LINES: usize = 2;
    /// The largest changed region compared line by line. Beyond this the
    /// region is reported as one wholesale replacement rather than spending
    /// quadratic time on a document nobody can read row by row anyway.
    const MAX_ALIGNED_LINES: usize = 2_000;

    /// Compares the buffer against the source's content.
    pub fn new(mine: &str, source: &str) -> Self {
        let mine_lines: Vec<&str> = mine.lines().collect();
        let source_lines: Vec<&str> = source.lines().collect();

        let prefix = common_prefix(&mine_lines, &source_lines);
        let suffix = common_suffix(&mine_lines[prefix..], &source_lines[prefix..]);
        let mine_middle = prefix..mine_lines.len() - suffix;
        let source_middle = prefix..source_lines.len() - suffix;

        let (pairs, truncated) = align(
            &mine_lines,
            &source_lines,
            mine_middle,
            source_middle,
            prefix,
        );

        let mut rows = Vec::new();
        let mut change_count = 0;
        let mut unchanged_run: Vec<LineComparisonRow> = Vec::new();

        for pair in pairs {
            let changed = matches!(
                (&pair.0, &pair.1),
                (
                    Some(DiffLine {
                        change: LineChange::Removed,
                        ..
                    }),
                    _
                ) | (
                    _,
                    Some(DiffLine {
                        change: LineChange::Added,
                        ..
                    })
                )
            );
            let row = LineComparisonRow::Pair {
                left: pair.0,
                right: pair.1,
            };
            if changed {
                change_count += 1;
                rows.extend(fold(std::mem::take(&mut unchanged_run), true));
                rows.push(row);
            } else {
                unchanged_run.push(row);
            }
        }
        rows.extend(fold(unchanged_run, false));

        Self {
            rows,
            change_count,
            truncated,
        }
    }

    pub fn rows(&self) -> &[LineComparisonRow] {
        &self.rows
    }

    /// The number of changed lines, which is what the footer states.
    pub const fn change_count(&self) -> usize {
        self.change_count
    }

    pub fn is_identical(&self) -> bool {
        self.change_count == 0
    }

    /// Whether the changed region was too large to align line by line, which
    /// the UI must say rather than implying a precise comparison.
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// The row indices holding a change, for Previous/Next navigation.
    pub fn change_rows(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| match row {
                LineComparisonRow::Pair { left, right } => {
                    let changed = left
                        .as_ref()
                        .is_some_and(|line| line.change == LineChange::Removed)
                        || right
                            .as_ref()
                            .is_some_and(|line| line.change == LineChange::Added);
                    changed.then_some(index)
                }
                LineComparisonRow::Collapsed { .. } => None,
            })
            .collect()
    }
}

type Pair = (Option<DiffLine>, Option<DiffLine>);

/// Emits unchanged rows, folding a long run into a single collapsed row.
///
/// `leading` keeps context *before* a change; the tail of the document keeps
/// context after the last one.
fn fold(run: Vec<LineComparisonRow>, leading: bool) -> Vec<LineComparisonRow> {
    let context = LineComparison::CONTEXT_LINES;
    if run.len() <= context * 2 + 1 {
        return run;
    }
    let mut folded = Vec::with_capacity(context * 2 + 1);
    let (head, tail) = run.split_at(if leading {
        run.len() - context
    } else {
        context
    });
    if leading {
        folded.push(LineComparisonRow::Collapsed { lines: head.len() });
        folded.extend_from_slice(tail);
    } else {
        folded.extend_from_slice(head);
        folded.push(LineComparisonRow::Collapsed { lines: tail.len() });
    }
    folded
}

/// Pairs the two sides up, emitting equal lines as pairs and the differing
/// middle as removals beside additions.
fn align(
    mine: &[&str],
    source: &[&str],
    mine_middle: Range<usize>,
    source_middle: Range<usize>,
    prefix: usize,
) -> (Vec<Pair>, bool) {
    let mut pairs: Vec<Pair> = Vec::new();
    for index in 0..prefix {
        pairs.push((
            Some(unchanged(index, mine[index])),
            Some(unchanged(index, source[index])),
        ));
    }

    let mine_middle_lines = &mine[mine_middle.clone()];
    let source_middle_lines = &source[source_middle.clone()];
    let too_large = mine_middle_lines.len() > LineComparison::MAX_ALIGNED_LINES
        || source_middle_lines.len() > LineComparison::MAX_ALIGNED_LINES;

    let script = if too_large {
        // One wholesale replacement: every line of mine removed, every line of
        // the source added.
        mine_middle_lines
            .iter()
            .enumerate()
            .map(|(offset, _)| Step::Remove(mine_middle.start + offset))
            .chain(
                source_middle_lines
                    .iter()
                    .enumerate()
                    .map(|(offset, _)| Step::Add(source_middle.start + offset)),
            )
            .collect()
    } else {
        longest_common_subsequence(
            mine_middle_lines,
            source_middle_lines,
            mine_middle.start,
            source_middle.start,
        )
    };

    let mut pending_removals: Vec<DiffLine> = Vec::new();
    let mut pending_additions: Vec<DiffLine> = Vec::new();
    for step in script {
        match step {
            Step::Remove(index) => pending_removals.push(DiffLine {
                number: index + 1,
                text: mine[index].to_owned(),
                change: LineChange::Removed,
            }),
            Step::Add(index) => pending_additions.push(DiffLine {
                number: index + 1,
                text: source[index].to_owned(),
                change: LineChange::Added,
            }),
            Step::Keep(mine_index, source_index) => {
                drain_pending(&mut pairs, &mut pending_removals, &mut pending_additions);
                pairs.push((
                    Some(unchanged(mine_index, mine[mine_index])),
                    Some(unchanged(source_index, source[source_index])),
                ));
            }
        }
    }
    drain_pending(&mut pairs, &mut pending_removals, &mut pending_additions);

    for offset in 0..(mine.len() - mine_middle.end) {
        let mine_index = mine_middle.end + offset;
        let source_index = source_middle.end + offset;
        pairs.push((
            Some(unchanged(mine_index, mine[mine_index])),
            Some(unchanged(source_index, source[source_index])),
        ));
    }

    (pairs, too_large)
}

/// Lays removals beside additions so a changed line reads as one row.
fn drain_pending(
    pairs: &mut Vec<Pair>,
    removals: &mut Vec<DiffLine>,
    additions: &mut Vec<DiffLine>,
) {
    let rows = removals.len().max(additions.len());
    let mut removals = removals.drain(..);
    let mut additions = additions.drain(..);
    for _ in 0..rows {
        pairs.push((removals.next(), additions.next()));
    }
}

fn unchanged(index: usize, text: &str) -> DiffLine {
    DiffLine {
        number: index + 1,
        text: text.to_owned(),
        change: LineChange::Unchanged,
    }
}

enum Step {
    Keep(usize, usize),
    Remove(usize),
    Add(usize),
}

/// A classic LCS table over the differing middle only, which is what keeps the
/// quadratic cost bounded in practice.
fn longest_common_subsequence(
    mine: &[&str],
    source: &[&str],
    mine_offset: usize,
    source_offset: usize,
) -> Vec<Step> {
    let rows = mine.len();
    let columns = source.len();
    let mut table = vec![0_u32; (rows + 1) * (columns + 1)];
    let at = |row: usize, column: usize| row * (columns + 1) + column;
    for row in (0..rows).rev() {
        for column in (0..columns).rev() {
            table[at(row, column)] = if mine[row] == source[column] {
                table[at(row + 1, column + 1)] + 1
            } else {
                table[at(row + 1, column)].max(table[at(row, column + 1)])
            };
        }
    }

    let mut steps = Vec::new();
    let (mut row, mut column) = (0, 0);
    while row < rows && column < columns {
        if mine[row] == source[column] {
            steps.push(Step::Keep(mine_offset + row, source_offset + column));
            row += 1;
            column += 1;
        } else if table[at(row + 1, column)] >= table[at(row, column + 1)] {
            steps.push(Step::Remove(mine_offset + row));
            row += 1;
        } else {
            steps.push(Step::Add(source_offset + column));
            column += 1;
        }
    }
    steps.extend((row..rows).map(|index| Step::Remove(mine_offset + index)));
    steps.extend((column..columns).map(|index| Step::Add(source_offset + index)));
    steps
}

fn common_prefix(left: &[&str], right: &[&str]) -> usize {
    left.iter()
        .zip(right.iter())
        .take_while(|(one, other)| one == other)
        .count()
}

fn common_suffix(left: &[&str], right: &[&str]) -> usize {
    left.iter()
        .rev()
        .zip(right.iter().rev())
        .take_while(|(one, other)| one == other)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed_pairs(comparison: &LineComparison) -> Vec<(Option<String>, Option<String>)> {
        comparison
            .rows()
            .iter()
            .filter_map(|row| match row {
                LineComparisonRow::Pair { left, right } => {
                    let changed = left
                        .as_ref()
                        .is_some_and(|line| line.change != LineChange::Unchanged)
                        || right
                            .as_ref()
                            .is_some_and(|line| line.change != LineChange::Unchanged);
                    changed.then(|| {
                        (
                            left.as_ref().map(|line| line.text.clone()),
                            right.as_ref().map(|line| line.text.clone()),
                        )
                    })
                }
                LineComparisonRow::Collapsed { .. } => None,
            })
            .collect()
    }

    #[test]
    fn identical_text_has_no_changes() {
        let comparison = LineComparison::new("alpha\nbeta\n", "alpha\nbeta\n");
        assert!(comparison.is_identical());
        assert_eq!(comparison.change_count(), 0);
        assert!(comparison.change_rows().is_empty());
    }

    #[test]
    fn a_modified_line_is_a_removal_beside_an_addition() {
        let comparison = LineComparison::new("alpha\nbeta\n", "alpha\nBETA\n");
        assert_eq!(
            changed_pairs(&comparison),
            [(Some("beta".to_owned()), Some("BETA".to_owned()))]
        );
        assert_eq!(comparison.change_count(), 1);
    }

    #[test]
    fn an_inserted_line_has_an_empty_left_side() {
        let comparison = LineComparison::new("alpha\ngamma\n", "alpha\nbeta\ngamma\n");
        assert_eq!(
            changed_pairs(&comparison),
            [(None, Some("beta".to_owned()))]
        );
    }

    #[test]
    fn a_deleted_line_has_an_empty_right_side() {
        let comparison = LineComparison::new("alpha\nbeta\ngamma\n", "alpha\ngamma\n");
        assert_eq!(
            changed_pairs(&comparison),
            [(Some("beta".to_owned()), None)]
        );
    }

    #[test]
    fn line_numbers_are_one_based_and_per_side() {
        let comparison = LineComparison::new("a\nb\nc\n", "a\nc\n");
        let LineComparisonRow::Pair { left, right } = &comparison.rows()[0] else {
            panic!("the first row should be a pair");
        };
        assert_eq!(left.as_ref().unwrap().number, 1);
        assert_eq!(right.as_ref().unwrap().number, 1);

        let removed = comparison
            .rows()
            .iter()
            .find_map(|row| match row {
                LineComparisonRow::Pair { left, .. } => left
                    .as_ref()
                    .filter(|line| line.change == LineChange::Removed)
                    .cloned(),
                LineComparisonRow::Collapsed { .. } => None,
            })
            .expect("a removed line");
        assert_eq!(removed.number, 2);
        assert_eq!(removed.text, "b");
    }

    #[test]
    fn every_changed_line_carries_a_marker() {
        let comparison = LineComparison::new("alpha\nbeta\n", "alpha\nBETA\n");
        for row in comparison.rows() {
            let LineComparisonRow::Pair { left, right } = row else {
                continue;
            };
            if let Some(line) = left {
                assert_eq!(
                    line.change.marker(),
                    if line.change == LineChange::Removed {
                        "-"
                    } else {
                        " "
                    }
                );
            }
            if let Some(line) = right {
                assert_eq!(
                    line.change.marker(),
                    if line.change == LineChange::Added {
                        "+"
                    } else {
                        " "
                    }
                );
            }
        }
    }

    #[test]
    fn a_long_unchanged_run_is_collapsed_with_its_count() {
        let mut mine = String::from("changed\n");
        for index in 0..20 {
            mine.push_str(&format!("line {index}\n"));
        }
        let source = mine.replacen("changed", "CHANGED", 1);
        let comparison = LineComparison::new(&mine, &source);
        let collapsed: Vec<usize> = comparison
            .rows()
            .iter()
            .filter_map(|row| match row {
                LineComparisonRow::Collapsed { lines } => Some(*lines),
                LineComparisonRow::Pair { .. } => None,
            })
            .collect();
        assert_eq!(collapsed, [18]);
        assert_eq!(comparison.change_count(), 1);
    }

    #[test]
    fn a_short_unchanged_run_between_changes_is_kept() {
        let comparison = LineComparison::new("a\nkeep\nb\n", "A\nkeep\nB\n");
        assert!(comparison
            .rows()
            .iter()
            .all(|row| !matches!(row, LineComparisonRow::Collapsed { .. })));
        assert_eq!(comparison.change_count(), 2);
    }

    #[test]
    fn change_rows_point_at_every_change_for_navigation() {
        let comparison = LineComparison::new("a\nb\nc\nd\n", "A\nb\nc\nD\n");
        let rows = comparison.change_rows();
        assert_eq!(rows.len(), 2);
        for index in rows {
            assert!(matches!(
                comparison.rows()[index],
                LineComparisonRow::Pair { .. }
            ));
        }
    }

    #[test]
    fn an_enormous_changed_region_is_reported_rather_than_aligned() {
        let mine: String = (0..LineComparison::MAX_ALIGNED_LINES + 10)
            .map(|index| format!("mine {index}\n"))
            .collect();
        let source: String = (0..LineComparison::MAX_ALIGNED_LINES + 10)
            .map(|index| format!("source {index}\n"))
            .collect();
        let comparison = LineComparison::new(&mine, &source);
        assert!(comparison.truncated());
        assert!(comparison.change_count() > 0);
    }

    #[test]
    fn comparing_against_an_empty_source_removes_everything() {
        let comparison = LineComparison::new("alpha\nbeta\n", "");
        assert_eq!(comparison.change_count(), 2);
        assert!(changed_pairs(&comparison)
            .iter()
            .all(|(left, right)| left.is_some() && right.is_none()));
    }

    #[test]
    fn pane_headings_say_where_each_version_is() {
        assert_eq!(DiffSide::Mine.heading(true), "Your version · unsaved");
        assert_eq!(DiffSide::Source.heading(true), "On the remote host");
        assert_eq!(DiffSide::Source.heading(false), "On disk");
    }
}
