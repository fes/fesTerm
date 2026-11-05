use std::alloc::System;

use festerm_core::{Dimensions, Terminal};
use festerm_ui_egui::{
    selection_text_with_limit, CellPosition, SelectedTextError, Selection, SmartSelectionScope,
    TerminalSnapshot, TerminalView,
};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn bounded_selection_preflights_without_allocating_rows_or_partial_output() {
    let dimensions = Dimensions::new(1024, 8).unwrap();
    let mut terminal = Terminal::new(dimensions).unwrap();
    let grapheme = format!("e{}", "\u{301}".repeat(30));
    terminal.ingest(grapheme.repeat(dimensions.cell_count()).as_bytes());
    let snapshot = TerminalSnapshot::from_terminal(&terminal);
    let mut selection = Selection::default();
    selection.begin(CellPosition { column: 0, row: 0 });
    selection.extend(CellPosition {
        column: 1023,
        row: 7,
    });
    selection.finish();

    let region = Region::new(GLOBAL);
    let result = selection_text_with_limit(snapshot, &selection, 64);
    let allocations = region.change();
    assert_eq!(result, Err(SelectedTextError::MaxBytesExceeded));
    assert_eq!(allocations.allocations, 0);
    assert_eq!(allocations.reallocations, 0);

    selection.begin(CellPosition { column: 0, row: 0 });
    selection.extend(CellPosition { column: 1, row: 0 });
    selection.finish();
    let limit = grapheme.len() * 2;
    let region = Region::new(GLOBAL);
    let result = selection_text_with_limit(snapshot, &selection, limit);
    let allocations = region.change();
    assert_eq!(result.unwrap().unwrap(), grapheme.repeat(2));
    assert_eq!(allocations.allocations, 1);
    assert_eq!(allocations.reallocations, 0);
    assert_eq!(allocations.bytes_allocated, limit);

    let mut terminal = Terminal::new(Dimensions::new(20, 2).unwrap()).unwrap();
    terminal.ingest(b"Original sentence.\r");
    let mut view = TerminalView::default();
    view.smart_select_at_target(&terminal, SmartSelectionScope::Sentence);
    let region = Region::new(GLOBAL);
    let result = view.selected_text_with_limit(&terminal, 2);
    let allocations = region.change();
    assert_eq!(result, Err(SelectedTextError::MaxBytesExceeded));
    assert_eq!(allocations.allocations, 0);
    assert_eq!(allocations.reallocations, 0);

    terminal.ingest(format!("O{}", "\u{301}".repeat(120)).as_bytes());
    let region = Region::new(GLOBAL);
    let result = view.selected_text_with_limit(&terminal, 2);
    let allocations = region.change();
    assert_eq!(result, Ok(None));
    assert_eq!(allocations.allocations, 0);
    assert_eq!(allocations.reallocations, 0);
}
