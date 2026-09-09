//! Spreadsheet (.xlsx) extraction via calamine.
//!
//! Each non-empty worksheet becomes one Markdown page: the sheet name as a
//! heading, then the used range as a pipe table. Output is capped at
//! [`MAX_SHEET_ROWS`] × [`MAX_SHEET_COLS`] per sheet with an explicit elision
//! marker, so a hundred-thousand-row workbook cannot flood distillation.
//! Formula cells surface as their cached values (calamine reads values, not
//! formulas), and empty cells collapse to empty table cells.
//!
//! anydoc also reads xlsx, but it renders every cell of every sheet into a
//! single page with no row or column cap, so spreadsheets route through this
//! capped renderer instead.

use std::fs::File;
use std::io::BufReader;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use calamine::{Data, DataRef, Reader, Xlsx};

use crate::extract::{
    CancellationToken, ExtractedDocument, ExtractedPage, ExtractionError, ExtractionWarning,
    PageSource, enforce_office_decompressed_limit,
};
use crate::limits::ResourceLimits;

/// Rows rendered per sheet before elision.
pub const MAX_SHEET_ROWS: usize = 200;
/// Columns rendered per sheet before elision.
pub const MAX_SHEET_COLS: usize = 30;

/// How many cells may pile up before the window is pruned again. Pruning on
/// every cell would be quadratic; pruning when the buffer reaches a few times
/// the rendered window keeps the cost amortised and the memory bounded.
const CELLS_BEFORE_PRUNE: usize = 4 * MAX_SHEET_ROWS * MAX_SHEET_COLS;

/// Runs one calamine operation behind a panic barrier: calamine can panic on
/// crafted or corrupt containers, and a dependency panic must degrade to a
/// parse error that routes the document to review. `AssertUnwindSafe` is
/// sound because a caught panic always propagates as an error, so the
/// workbook is never used again.
fn contained<T>(operation: &str, run: impl FnOnce() -> T) -> Result<T, ExtractionError> {
    catch_unwind(AssertUnwindSafe(run)).map_err(|_| {
        ExtractionError::parse_failed(format!("workbook parser aborted during {operation}"))
    })
}

/// The part of a worksheet that will be rendered, plus the corners of the
/// used range the elision marker counts against.
struct CappedSheet {
    /// Absolute `(row, column, text)` of every cell inside the rendered window.
    cells: Vec<(u32, u32, String)>,
    start: (u32, u32),
    end: (u32, u32),
}

pub fn extract_xlsx(
    path: &Path,
    limits: &ResourceLimits,
    cancel: &CancellationToken,
) -> Result<ExtractedDocument, ExtractionError> {
    cancel.check()?;
    let metadata = std::fs::metadata(path).map_err(ExtractionError::io)?;
    limits.validate_source_size(metadata.len())?;
    enforce_office_decompressed_limit(path, limits, cancel)?;
    cancel.check()?;

    let mut workbook: Xlsx<BufReader<File>> = contained("workbook open", || {
        calamine::open_workbook(path)
    })?
    .map_err(|error| ExtractionError::parse_failed(format!("workbook did not open: {error}")))?;
    let sheet_names = contained("sheet listing", || workbook.sheet_names())?;
    limits.validate_page_count(sheet_names.len())?;

    let mut pages = Vec::new();
    let mut truncated = false;
    for name in &sheet_names {
        cancel.check()?;
        let sheet = contained("worksheet read", || {
            read_capped_sheet(&mut workbook, name, cancel)
        })??;
        let Some(sheet) = sheet else {
            continue;
        };
        let (text, sheet_truncated) = render_sheet(name, &sheet);
        truncated |= sheet_truncated;
        pages.push(ExtractedPage {
            page_number: pages.len() + 1,
            text,
            source: PageSource::AnyDoc,
            ocr_confidence: None,
            vision_escalated: false,
        });
    }
    if pages.is_empty() {
        return Err(ExtractionError::unsupported(
            "workbook contains no readable cells",
        ));
    }
    Ok(ExtractedDocument {
        pages,
        warnings: if truncated {
            vec![ExtractionWarning::TextTruncated]
        } else {
            vec![]
        },
        truncated,
        optional_image: None,
    })
}

/// Streams one worksheet's cells and keeps only the ones the cap can render.
///
/// `worksheet_range` materialises the used range densely — one slot per cell
/// of the bounding box — before any cap of ours applies. A three-kilobyte
/// workbook whose only two cells are `A1` and `XFD1048576` therefore asks the
/// allocator for half a terabyte and aborts the process, which no panic
/// barrier can turn back into a parse error. Reading cell by cell keeps at
/// most a few thousand of them, whatever the sheet claims its corners are.
///
/// `None` is a sheet with no cell values at all, which the caller skips.
fn read_capped_sheet(
    workbook: &mut Xlsx<BufReader<File>>,
    name: &str,
    cancel: &CancellationToken,
) -> Result<Option<CappedSheet>, ExtractionError> {
    let read_error = |error: calamine::XlsxError| {
        ExtractionError::parse_failed(format!("worksheet {name:?} did not read: {error}"))
    };
    let mut reader = workbook.worksheet_cells_reader(name).map_err(read_error)?;
    let mut cells: Vec<(u32, u32, String)> = Vec::new();
    let mut start: Option<(u32, u32)> = None;
    let mut end = (0_u32, 0_u32);
    let mut seen = 0_u64;
    while let Some(cell) = reader.next_cell().map_err(read_error)? {
        if seen % 1_024 == 0 {
            cancel.check()?;
        }
        seen += 1;
        if matches!(cell.get_value(), DataRef::Empty) {
            continue;
        }
        let (row, column) = cell.get_position();
        let corner = start.get_or_insert((row, column));
        corner.0 = corner.0.min(row);
        corner.1 = corner.1.min(column);
        end = (end.0.max(row), end.1.max(column));
        let value: Data = cell.get_value().clone().into();
        cells.push((row, column, cell_text(&value)));
        if cells.len() >= CELLS_BEFORE_PRUNE {
            prune_to_window(&mut cells, *corner);
        }
    }
    let Some(start) = start else {
        return Ok(None);
    };
    prune_to_window(&mut cells, start);
    Ok(Some(CappedSheet { cells, start, end }))
}

/// Drops every cell outside the rows and columns the renderer can show. A
/// cell is unreachable once it sits more than a windowful past the top-left
/// corner, and that corner only ever moves up and to the left.
fn prune_to_window(cells: &mut Vec<(u32, u32, String)>, start: (u32, u32)) {
    cells.retain(|(row, column, _)| {
        (row - start.0) < MAX_SHEET_ROWS as u32 && (column - start.1) < MAX_SHEET_COLS as u32
    });
}

/// Renders one worksheet as `## name` plus a pipe table, returning the text
/// and whether any rows or columns were elided.
///
/// Rows and columns past the last cell inside the window count as elided
/// rather than being drawn as empty table cells: a sheet whose used range
/// runs out to one stray far-away cell would otherwise render two hundred
/// blank rows before admitting it left anything out.
fn render_sheet(name: &str, sheet: &CappedSheet) -> (String, bool) {
    let height = u64::from(sheet.end.0 - sheet.start.0) + 1;
    let width = u64::from(sheet.end.1 - sheet.start.1) + 1;
    let shown_rows = sheet
        .cells
        .iter()
        .map(|(row, _, _)| (row - sheet.start.0) as usize + 1)
        .max()
        .unwrap_or(0);
    let shown_cols = sheet
        .cells
        .iter()
        .map(|(_, column, _)| (column - sheet.start.1) as usize + 1)
        .max()
        .unwrap_or(0);
    let mut grid = vec![vec![String::new(); shown_cols]; shown_rows];
    for (row, column, value) in &sheet.cells {
        grid[(row - sheet.start.0) as usize][(column - sheet.start.1) as usize] = value.clone();
    }

    let mut text = format!("## {}\n\n", sanitize_cell(name));
    for (index, row) in grid.iter().enumerate() {
        text.push_str(&format!("| {} |\n", row.join(" | ")));
        if index == 0 {
            text.push_str(&format!("| {} |\n", vec!["---"; shown_cols].join(" | ")));
        }
    }

    let hidden_rows = height - shown_rows as u64;
    let hidden_cols = width - shown_cols as u64;
    let elided = hidden_rows > 0 || hidden_cols > 0;
    if elided {
        let marker = match (hidden_rows, hidden_cols) {
            (rows, 0) => format!("[... {rows} more rows not shown]"),
            (0, cols) => format!("[... {cols} more columns not shown]"),
            (rows, cols) => format!("[... {rows} more rows and {cols} more columns not shown]"),
        };
        text.push('\n');
        text.push_str(&marker);
        text.push('\n');
    }
    (text, elided)
}

fn cell_text(data: &Data) -> String {
    match data {
        Data::Empty => String::new(),
        Data::String(value) | Data::DateTimeIso(value) | Data::DurationIso(value) => {
            sanitize_cell(value)
        }
        Data::Int(value) => value.to_string(),
        Data::Float(value) => value.to_string(),
        Data::Bool(value) => if *value { "TRUE" } else { "FALSE" }.to_owned(),
        Data::DateTime(value) => {
            if value.is_datetime() {
                let (year, month, day, hour, minute, second, _) = value.to_ymd_hms_milli();
                if hour == 0 && minute == 0 && second == 0 {
                    format!("{year:04}-{month:02}-{day:02}")
                } else {
                    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
                }
            } else {
                let total_seconds = (value.as_f64() * 86_400.0).round() as i64;
                format!(
                    "{}:{:02}:{:02}",
                    total_seconds / 3_600,
                    total_seconds % 3_600 / 60,
                    total_seconds % 60
                )
            }
        }
        Data::Error(error) => error.to_string(),
    }
}

/// Keeps a value on one table line: newlines become spaces and pipes are
/// escaped so a cell cannot break the row it sits in.
fn sanitize_cell(value: &str) -> String {
    value
        .replace(['\r', '\n'], " ")
        .replace('|', "\\|")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{ExcelDateTime, ExcelDateTimeType};

    #[test]
    fn cell_values_render_deterministically_for_every_data_variant() {
        assert_eq!(cell_text(&Data::Empty), "");
        assert_eq!(cell_text(&Data::String("a | b\nc".to_owned())), "a \\| b c");
        assert_eq!(cell_text(&Data::Int(-3)), "-3");
        assert_eq!(cell_text(&Data::Float(1.5)), "1.5");
        assert_eq!(cell_text(&Data::Bool(true)), "TRUE");
    }

    #[test]
    fn excel_serial_datetimes_render_as_iso_dates_and_times() {
        let date = ExcelDateTime::new(45_943.0, ExcelDateTimeType::DateTime, false);
        assert_eq!(cell_text(&Data::DateTime(date)), "2025-10-13");
        let datetime = ExcelDateTime::new(45_943.5, ExcelDateTimeType::DateTime, false);
        assert_eq!(cell_text(&Data::DateTime(datetime)), "2025-10-13 12:00:00");
        let duration = ExcelDateTime::new(1.25, ExcelDateTimeType::TimeDelta, false);
        assert_eq!(cell_text(&Data::DateTime(duration)), "30:00:00");
    }
}
