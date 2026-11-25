use pdf_inspector::extract_pages_markdown_mem;

const TAGGED_STATEMENT: &[u8] = include_bytes!("fixtures/chrome_compact_tagged_statement.pdf");

fn assert_transaction_columns(markdown: &str) {
    assert!(
        markdown.contains("|10/01|LOAN PMT|2,675.12||15,777.25|"),
        "withdrawal row lost its empty Deposits cell:\n{markdown}"
    );
    assert!(
        markdown.contains("|10/06|MOBILE DEPOSIT||6,146.48|21,923.73|"),
        "deposit row lost its empty Withdrawals cell:\n{markdown}"
    );
}

#[test]
fn all_pages_markdown_keeps_tagged_cells_aligned() {
    let result = extract_pages_markdown_mem(TAGGED_STATEMENT, None)
        .expect("extract all pages from the tagged statement");
    assert_eq!(result.pages.len(), 1);
    assert_transaction_columns(&result.pages[0].markdown);
}

#[test]
fn selected_page_markdown_keeps_tagged_cells_aligned() {
    let result = extract_pages_markdown_mem(TAGGED_STATEMENT, Some(&[0]))
        .expect("extract the selected tagged statement page");
    assert_eq!(result.pages.len(), 1);
    assert_transaction_columns(&result.pages[0].markdown);
}
