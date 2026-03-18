//! Financial token splitting for consolidated value items.

use crate::types::TextItem;

/// Check if a whitespace-separated token looks like a financial number.
/// Must contain at least one digit; all chars must be `0-9 , . ( ) - + %`.
pub(crate) fn is_numeric_token(tok: &str) -> bool {
    if tok.is_empty() {
        return false;
    }
    let mut has_digit = false;
    for c in tok.chars() {
        match c {
            '0'..='9' => has_digit = true,
            ',' | '.' | '(' | ')' | '-' | '+' | '%' => {}
            _ => return false,
        }
    }
    has_digit
}

/// Check for em-dash, en-dash, or minus used as nil marker in financial tables.
pub(crate) fn is_dash_token(tok: &str) -> bool {
    matches!(tok, "\u{2014}" | "\u{2013}" | "-" | "\u{2012}")
}

/// Returns true if text contains 2+ consecutive alphabetic characters.
/// Fast early-exit to reject items like `"Land $ 778,177"`.
pub(crate) fn has_alphabetic_words(text: &str) -> bool {
    let mut consecutive = 0u32;
    for c in text.chars() {
        if c.is_alphabetic() {
            consecutive += 1;
            if consecutive >= 2 {
                return true;
            }
        } else {
            consecutive = 0;
        }
    }
    false
}

/// Splits text by whitespace, then groups tokens into financial values.
/// - `$` + numeric token → one value (`"$ 5,147,649"`)
/// - standalone numeric token → one value (`"114,167"`)
/// - dash token → one value (`"—"`)
/// - any unrecognized token → return `None` (not a pure-value item)
pub(crate) fn tokenize_financial_values(text: &str) -> Option<Vec<String>> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let mut values = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok == "$" {
            // Dollar sign followed by a numeric token → one value
            if i + 1 < tokens.len() && is_numeric_token(tokens[i + 1]) {
                values.push(format!("{} {}", tok, tokens[i + 1]));
                i += 2;
            } else {
                return None;
            }
        } else if is_numeric_token(tok) || is_dash_token(tok) {
            values.push(tok.to_string());
            i += 1;
        } else {
            return None;
        }
    }
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

/// Try to split a consolidated financial item into individual sub-items.
/// Criteria: width > font_size × 20, no alphabetic words, tokenization yields 3+ values.
/// Creates sub-items with evenly-distributed X positions across the original item's span.
pub(crate) fn try_split_financial_item(item: &TextItem) -> Option<Vec<TextItem>> {
    if item.width <= item.font_size * 20.0 {
        return None;
    }
    let text = &item.text;
    if has_alphabetic_words(text) {
        return None;
    }
    let values = tokenize_financial_values(text)?;
    if values.len() < 3 {
        return None;
    }
    let n = values.len() as f32;
    let spacing = item.width / n;
    let sub_width = spacing * 0.9;
    let mut sub_items = Vec::with_capacity(values.len());
    for (i, val) in values.iter().enumerate() {
        sub_items.push(TextItem {
            text: val.clone(),
            x: item.x + spacing * i as f32 + spacing * 0.5,
            y: item.y,
            width: sub_width,
            height: item.height,
            font: item.font.clone(),
            font_size: item.font_size,
            page: item.page,
            is_bold: item.is_bold,
            is_italic: item.is_italic,
            item_type: item.item_type.clone(),
            mcid: item.mcid,
        });
    }
    Some(sub_items)
}
