//! The emit pass: pages extract one at a time and classify against
//! the document-wide calibration — tables first (structure tree, then
//! rects, then ruled lines, then the heuristic detector, first claim
//! wins), then lines into headings, list items, code, captions, and
//! paragraphs. Furniture and folio lines are emitted FLAGGED, never
//! deleted, so a consumer can demote them the way the HTML lane
//! demotes chrome. A table-only page holds its table back one page so
//! a continuation on the next page merges into it.

use std::collections::{HashMap, HashSet};

use lopdf::Document;

use super::evidence::{folio_pattern, strip_edge_digits, Calibration};
use crate::extractor::{extract_page_unit, group_into_lines, FontStyleCache, PageVecSizes};
use crate::markdown::{
    chart_regions_by_page, effective_heading_level, is_caption_line, is_list_item, join_decision,
    line_is_monospace, merge_heading_lines, resolve_line_struct_role, strip_edge_furniture, Join,
    MarkdownOptions,
};
use crate::structure_tree::{StructRole, StructTable};
use crate::tables::{
    content_width, detect_tables_from_lines, detect_tables_from_rects,
    detect_tables_from_struct_tree, detect_tables_with_page_width, Table,
};
use crate::tounicode::FontCMaps;
use crate::types::{ItemType, TextItem, TextLine};
use crate::PdfError;

/// One classified region of a page. `text` is PLAIN — no markup
/// syntax; list items keep their printed marker glyphs, tables carry
/// their cell text joined in reading order with the grid in `table`.
#[derive(Debug, Clone)]
pub struct PdfBlock {
    /// 1-indexed page; a table merged across pages carries its FIRST
    /// page, and a list item straddling a page break its starting page.
    pub page: u32,
    pub kind: PdfBlockKind,
    pub text: String,
    /// Heading level 1-6; None on other kinds.
    pub level: Option<u8>,
    /// Running header/footer or folio: kept for display and layout
    /// consumers, demoted from body text.
    pub furniture: bool,
    /// Union of the block's line boxes (x0, y0, x1, y1) in the page's
    /// extraction coordinate space. Rotated pages keep the extractor's
    /// untransformed coordinates; a merged continuation table keeps
    /// its first page's box.
    pub bbox: (f32, f32, f32, f32),
    pub table: Option<PdfTable>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfBlockKind {
    Heading,
    Paragraph,
    ListItem,
    Code,
    Table,
    Image,
    Caption,
}

#[derive(Debug, Clone)]
pub struct PdfTable {
    pub n_rows: u32,
    pub n_cols: u16,
    pub cells: Vec<PdfTableCell>,
}

#[derive(Debug, Clone)]
pub struct PdfTableCell {
    pub text: String,
    pub row: u32,
    pub col: u16,
    /// Row 0 is the header row, matching how detected tables render.
    pub is_header: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_document(
    doc: &Document,
    font_cmaps: &FontCMaps,
    page_filter: Option<&HashSet<u32>>,
    include_invisible: bool,
    calibration: &Calibration,
    struct_roles: Option<&HashMap<u32, HashMap<i64, StructRole>>>,
    struct_tables: &[StructTable],
    options: &MarkdownOptions,
) -> Result<Vec<PdfBlock>, PdfError> {
    let pages = doc.get_pages();
    let mut style_cache = FontStyleCache::new();
    let mut page_vec_sizes = PageVecSizes::default();
    let mut blocks: Vec<PdfBlock> = Vec::new();
    // A table-only page's single table waits one page for a
    // continuation before it is emitted.
    let mut open_table: Option<PdfBlock> = None;
    // List state survives page breaks: a wrapped item's continuation
    // line on the next page belongs to the item, not a new paragraph.
    let mut list_carry: Option<ListCarry> = None;

    for (page_num, &page_id) in pages.iter() {
        if let Some(filter) = page_filter {
            if !filter.contains(page_num) {
                continue;
            }
        }
        if calibration.garbage_ocr_pages.contains(page_num) {
            continue;
        }
        let unit = extract_page_unit(
            doc,
            page_id,
            *page_num,
            font_cmaps,
            include_invisible,
            &mut style_cache,
            &mut page_vec_sizes,
        )?;

        let page = emit_page(
            unit.items,
            &unit.rects,
            &unit.lines,
            *page_num,
            pages.len() as u32,
            calibration,
            struct_roles,
            struct_tables,
            options,
            &mut list_carry,
        );

        // Table continuation: a table-only page's single table either
        // extends the open chain (same column count) or starts one.
        let table_only = page.table_only;
        let mut page_blocks = page.blocks;
        if let Some(mut open) = open_table.take() {
            let continues = table_only
                && page_blocks.len() == 1
                && matches!(
                    (&open.table, &page_blocks[0].table),
                    (Some(a), Some(b)) if a.n_cols == b.n_cols
                );
            if continues {
                let continuation = page_blocks.remove(0);
                merge_table_rows(&mut open, continuation);
                open_table = Some(open);
                continue;
            }
            blocks.push(open);
        }
        if table_only && page_blocks.len() == 1 {
            open_table = page_blocks.pop();
            continue;
        }
        blocks.append(&mut page_blocks);
    }
    if let Some(open) = open_table.take() {
        blocks.push(open);
    }
    if let Some(carry) = list_carry.take() {
        blocks.push(carry.block);
    }
    Ok(blocks)
}

/// An unfinished list item awaiting a possible wrapped continuation
/// line (which may arrive after furniture lines or a page break).
/// Absorbing a line advances the position so a run of wrapped lines
/// joins one item while anything past the gap starts fresh.
struct ListCarry {
    block: PdfBlock,
    last_x: f32,
    last_page: u32,
    last_y: f32,
}

struct EmittedPage {
    blocks: Vec<PdfBlock>,
    /// True when the page carries exactly one table and no body text
    /// outside it — the shape a multi-page table's middle pages have.
    table_only: bool,
}

#[allow(clippy::too_many_arguments)]
fn emit_page(
    items: Vec<TextItem>,
    rects: &[crate::types::PdfRect],
    pdf_lines: &[crate::types::PdfLine],
    page: u32,
    doc_pages: u32,
    calibration: &Calibration,
    struct_roles: Option<&HashMap<u32, HashMap<i64, StructRole>>>,
    struct_tables: &[StructTable],
    options: &MarkdownOptions,
    list_carry: &mut Option<ListCarry>,
) -> EmittedPage {
    // Images and links: images become blocks only when the caller asked
    // for them; links ride inside their lines' text.
    let mut text_items: Vec<TextItem> = Vec::new();
    let mut image_blocks: Vec<PdfBlock> = Vec::new();
    for item in items {
        match item.item_type {
            ItemType::Image => {
                if options.include_images {
                    image_blocks.push(PdfBlock {
                        page,
                        kind: PdfBlockKind::Image,
                        text: String::new(),
                        level: None,
                        furniture: false,
                        bbox: (item.x, item.y, item.x + item.width, item.y + item.height),
                        table: None,
                    });
                }
            }
            // A link annotation's text is its target URI, not page
            // text; the visible anchor is its own text item. URIs stay
            // out of block text.
            ItemType::Link(_) => {}
            _ => text_items.push(item),
        }
    }

    // Chart text must not seed table hypotheses: the gutter fill fuses
    // columns. It still emits as ordinary lines below.
    let chart_regions = chart_regions_by_page(&text_items, rects, pdf_lines);
    let in_chart = |item: &TextItem| {
        chart_regions.get(&page).is_some_and(|regions| {
            regions.iter().any(|&(x0, y0, x1, y1)| {
                let cx = item.x + item.width / 2.0;
                item.y >= y0 && item.y <= y1 && cx >= x0 && cx <= x1
            })
        })
    };
    let detectable: Vec<usize> = (0..text_items.len())
        .filter(|&i| !in_chart(&text_items[i]))
        .collect();
    let detect_items: Vec<TextItem> = detectable.iter().map(|&i| text_items[i].clone()).collect();

    // Table detection cascade: strongest structural evidence first,
    // first claim on an item wins.
    let mut claimed: HashSet<usize> = HashSet::new();
    let mut table_blocks: Vec<PdfBlock> = Vec::new();
    let accept = |table: Table, claimed: &mut HashSet<usize>, table_blocks: &mut Vec<PdfBlock>| {
        if table
            .item_indices
            .iter()
            .any(|&i| claimed.contains(&detectable[i]))
        {
            return;
        }
        if table.cells.iter().map(|row| row.len()).sum::<usize>() == 0 {
            return;
        }
        for &i in &table.item_indices {
            claimed.insert(detectable[i]);
        }
        let claimed_items: Vec<&TextItem> = table
            .item_indices
            .iter()
            .map(|&i| &text_items[detectable[i]])
            .collect();
        table_blocks.push(table_block(&table, page, &claimed_items));
    };
    for table in detect_tables_from_struct_tree(&detect_items, struct_tables, page) {
        accept(table, &mut claimed, &mut table_blocks);
    }
    let (rect_tables, _hints) = detect_tables_from_rects(&detect_items, rects, page);
    for table in rect_tables {
        accept(table, &mut claimed, &mut table_blocks);
    }
    for table in detect_tables_from_lines(&detect_items, pdf_lines, page) {
        accept(table, &mut claimed, &mut table_blocks);
    }
    let page_width = content_width(&detect_items);
    for table in
        detect_tables_with_page_width(&detect_items, calibration.base_size, false, page_width)
    {
        accept(table, &mut claimed, &mut table_blocks);
    }

    // Everything unclaimed groups into lines and classifies.
    let free_items: Vec<TextItem> = text_items
        .iter()
        .enumerate()
        .filter(|(i, _)| !claimed.contains(i))
        .map(|(_, item)| item.clone())
        .collect();
    let lines = group_into_lines(free_items);
    // A heading wrapped over two physical lines is one heading.
    let lines = merge_heading_lines(
        lines,
        calibration.base_size,
        &calibration.heading_tiers,
        struct_roles,
    );
    // Documents too short for cross-page repetition evidence flag
    // their edge furniture positionally: whatever the drop-only edge
    // pass would remove is the furniture set.
    let edge_furniture: HashSet<(u32, String)> = if doc_pages < 3 {
        let kept: HashSet<(u32, String)> = strip_edge_furniture(lines.clone())
            .iter()
            .map(|l| (l.y.to_bits(), l.text()))
            .collect();
        lines
            .iter()
            .map(|l| (l.y.to_bits(), l.text()))
            .filter(|key| !kept.contains(key))
            .collect()
    } else {
        HashSet::new()
    };

    let mut line_blocks: Vec<PdfBlock> = Vec::new();
    let mut open_paragraph: Option<(PdfBlock, f32)> = None;
    let mut open_code: Option<(PdfBlock, f32)> = None;

    let flush_paragraph = |open: &mut Option<(PdfBlock, f32)>, out: &mut Vec<PdfBlock>| {
        if let Some((block, _)) = open.take() {
            out.push(block);
        }
    };

    for line in &lines {
        let text = line.text();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        let furniture = line_is_furniture(line, calibration)
            || edge_furniture.contains(&(line.y.to_bits(), text.clone()));
        let role = struct_roles.and_then(|roles| resolve_line_struct_role(line, roles));
        let heading_level = effective_heading_level(
            line,
            calibration.base_size,
            &calibration.heading_tiers,
            struct_roles,
        )
        .filter(|level| !calibration.suppressed_struct_levels.contains(level));
        let monospace = line_is_monospace(line);
        let caption = is_caption_line(trimmed) || matches!(role, Some(StructRole::Caption));

        // A wrapped list item's continuation: an unclassified line,
        // indented at or right of the item's text start, immediately
        // below the last absorbed line (or the first line after its
        // page break). Furniture lines are transparent to the carry.
        if !furniture {
            if let Some(carry) = list_carry.as_mut() {
                let first_x = line.items.first().map(|i| i.x).unwrap_or(0.0);
                let same_page_close = carry.last_page == line.page
                    && carry.last_y - line.y < calibration.paragraph_threshold * 1.5;
                let next_page_top = line.page == carry.last_page + 1;
                if !is_list_item(trimmed)
                    && heading_level.is_none()
                    && !monospace
                    && !caption
                    && first_x >= carry.last_x - 2.0
                    && (same_page_close || next_page_top)
                {
                    join_wrapped(&mut carry.block.text, trimmed, &calibration.vocabulary);
                    grow_bbox(&mut carry.block.bbox, line);
                    carry.last_page = line.page;
                    carry.last_y = line.y;
                    continue;
                }
                let done = list_carry.take().expect("carry checked above");
                line_blocks.push(done.block);
            }
        }

        if furniture {
            flush_paragraph(&mut open_paragraph, &mut line_blocks);
            flush_paragraph(&mut open_code, &mut line_blocks);
            line_blocks.push(line_block(line, page, PdfBlockKind::Paragraph, None, true));
            continue;
        }

        if monospace {
            flush_paragraph(&mut open_paragraph, &mut line_blocks);
            match open_code.as_mut() {
                Some((block, last_y)) if *last_y - line.y < calibration.paragraph_threshold => {
                    block.text.push('\n');
                    block.text.push_str(&text);
                    grow_bbox(&mut block.bbox, line);
                    *last_y = line.y;
                }
                _ => {
                    flush_paragraph(&mut open_code, &mut line_blocks);
                    open_code = Some((
                        line_block(line, page, PdfBlockKind::Code, None, false),
                        line.y,
                    ));
                }
            }
            continue;
        }
        flush_paragraph(&mut open_code, &mut line_blocks);

        if let Some(level) = heading_level {
            flush_paragraph(&mut open_paragraph, &mut line_blocks);
            line_blocks.push(line_block(
                line,
                page,
                PdfBlockKind::Heading,
                Some(level.min(6) as u8),
                false,
            ));
            continue;
        }

        let is_li =
            is_list_item(trimmed) || matches!(role, Some(StructRole::LI) | Some(StructRole::LBody));
        if is_li {
            flush_paragraph(&mut open_paragraph, &mut line_blocks);
            let block = line_block(line, page, PdfBlockKind::ListItem, None, false);
            let last_x = line.items.first().map(|i| i.x).unwrap_or(0.0);
            *list_carry = Some(ListCarry {
                block,
                last_x,
                last_page: line.page,
                last_y: line.y,
            });
            continue;
        }

        if caption {
            flush_paragraph(&mut open_paragraph, &mut line_blocks);
            line_blocks.push(line_block(line, page, PdfBlockKind::Caption, None, false));
            continue;
        }

        match open_paragraph.as_mut() {
            Some((block, last_y)) if *last_y - line.y < calibration.paragraph_threshold => {
                join_wrapped(&mut block.text, trimmed, &calibration.vocabulary);
                grow_bbox(&mut block.bbox, line);
                *last_y = line.y;
            }
            _ => {
                flush_paragraph(&mut open_paragraph, &mut line_blocks);
                open_paragraph = Some((
                    line_block(line, page, PdfBlockKind::Paragraph, None, false),
                    line.y,
                ));
            }
        }
    }
    flush_paragraph(&mut open_paragraph, &mut line_blocks);
    flush_paragraph(&mut open_code, &mut line_blocks);

    let body_text_blocks = line_blocks.iter().filter(|block| !block.furniture).count();
    let table_only = table_blocks.len() == 1 && body_text_blocks == 0 && image_blocks.is_empty();

    // Reading order within the page: top to bottom by each block's top
    // edge; the line classifier already yields column-aware order for
    // text, and tables slot in at their claimed position.
    let mut blocks =
        Vec::with_capacity(table_blocks.len() + line_blocks.len() + image_blocks.len());
    blocks.extend(table_blocks);
    blocks.extend(line_blocks);
    blocks.extend(image_blocks);
    blocks.sort_by(|a, b| b.bbox.3.total_cmp(&a.bbox.3));

    EmittedPage { blocks, table_only }
}

/// Join a wrapped line onto its block, repairing end-of-line
/// hyphenation with the document-as-dictionary policy: fragments seen
/// elsewhere joined plain fuse ("sec-" + "tion" -> "section"), known
/// or capitalized compounds keep their hyphen, and breaks with no
/// evidence stay visible rather than risk fusing column fragments.
fn join_wrapped(text: &mut String, next: &str, vocabulary: &super::evidence::Vocabulary) {
    let tail: String = text
        .chars()
        .rev()
        .skip(1)
        .take_while(|c| c.is_alphabetic())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let head: String = next.chars().take_while(|c| c.is_alphabetic()).collect();
    if text.ends_with('-') && tail.chars().count() >= 2 && head.chars().count() >= 2 {
        match join_decision(&tail, &head, &vocabulary.words, &vocabulary.hyphenated) {
            Join::Plain => {
                text.pop();
                text.push_str(next);
                return;
            }
            Join::Hyphen => {
                text.push_str(next);
                return;
            }
            Join::Keep => {}
        }
    }
    text.push(' ');
    text.push_str(next);
}

fn line_is_furniture(line: &TextLine, calibration: &Calibration) -> bool {
    let all_keyed = !line.items.is_empty()
        && line.items.iter().all(|item| {
            item.text.trim().is_empty()
                || calibration
                    .furniture_keys
                    .contains(&Calibration::furniture_key_of(item))
        });
    if all_keyed {
        return true;
    }
    if calibration.in_edge_band(line.page, line.y) {
        let text = line.text();
        let trimmed = text.trim();
        if calibration
            .furniture_line_texts
            .contains(&strip_edge_digits(trimmed))
        {
            return true;
        }
        if let Some(pattern) = folio_pattern(trimmed) {
            return calibration.folio_patterns.contains(&pattern);
        }
    }
    false
}

fn line_block(
    line: &TextLine,
    page: u32,
    kind: PdfBlockKind,
    level: Option<u8>,
    furniture: bool,
) -> PdfBlock {
    let mut bbox = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    grow_bbox_items(&mut bbox, &line.items);
    PdfBlock {
        page,
        kind,
        text: line.text().trim().to_string(),
        level,
        furniture,
        bbox,
        table: None,
    }
}

fn grow_bbox(bbox: &mut (f32, f32, f32, f32), line: &TextLine) {
    grow_bbox_items(bbox, &line.items);
}

fn grow_bbox_items(bbox: &mut (f32, f32, f32, f32), items: &[TextItem]) {
    for item in items {
        let top = item.y + item.height.max(item.font_size);
        bbox.0 = bbox.0.min(item.x);
        bbox.1 = bbox.1.min(item.y);
        bbox.2 = bbox.2.max(item.x + item.width);
        bbox.3 = bbox.3.max(top);
    }
}

fn table_block(table: &Table, page: u32, claimed_items: &[&TextItem]) -> PdfBlock {
    let n_rows = table.cells.len() as u32;
    let n_cols = table.cells.iter().map(|row| row.len()).max().unwrap_or(0) as u16;
    let mut cells = Vec::new();
    let mut text = String::new();
    for (row_idx, row) in table.cells.iter().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            if !cell.trim().is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(cell.trim());
            }
            cells.push(PdfTableCell {
                text: cell.clone(),
                row: row_idx as u32,
                col: col_idx as u16,
                is_header: row_idx == 0,
            });
        }
    }
    let mut bbox = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for item in claimed_items {
        let top = item.y + item.height.max(item.font_size);
        bbox.0 = bbox.0.min(item.x);
        bbox.1 = bbox.1.min(item.y);
        bbox.2 = bbox.2.max(item.x + item.width);
        bbox.3 = bbox.3.max(top);
    }
    PdfBlock {
        page,
        kind: PdfBlockKind::Table,
        text,
        level: None,
        furniture: false,
        bbox,
        table: Some(PdfTable {
            n_rows,
            n_cols,
            cells,
        }),
    }
}

/// Append a continuation page's data rows onto the open table. The
/// continuation's header row repeats the table's own header, so it is
/// dropped when identical; the merged block keeps the first page's
/// page number and box.
fn merge_table_rows(open: &mut PdfBlock, continuation: PdfBlock) {
    let (Some(open_table), Some(more)) = (open.table.as_mut(), continuation.table) else {
        return;
    };
    let header_of = |t: &PdfTable, row: u32| -> Vec<String> {
        t.cells
            .iter()
            .filter(|c| c.row == row)
            .map(|c| c.text.trim().to_string())
            .collect()
    };
    let skip_repeated_header = header_of(open_table, 0) == header_of(&more, 0);
    let base = open_table.n_rows;
    for cell in more.cells {
        if skip_repeated_header && cell.row == 0 {
            continue;
        }
        let offset = if skip_repeated_header { 1 } else { 0 };
        open_table.cells.push(PdfTableCell {
            text: cell.text,
            row: base + cell.row - offset,
            col: cell.col,
            is_header: false,
        });
    }
    open_table.n_rows = open_table
        .cells
        .iter()
        .map(|c| c.row + 1)
        .max()
        .unwrap_or(open_table.n_rows);
    open_table.n_cols = open_table.n_cols.max(more.n_cols);
    if !continuation.text.is_empty() {
        if !open.text.is_empty() {
            open.text.push(' ');
        }
        open.text.push_str(&continuation.text);
    }
}
