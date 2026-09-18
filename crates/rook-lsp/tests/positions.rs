#[test]
fn positions_use_utf16_and_empty_symbols_do_not_loop() {
    assert!(rook_lsp::locate("abc", "").is_none());
    let found = rook_lsp::locate("header\r\n😀 foo", "foo").unwrap();
    assert_eq!(found.line, 1);
    assert_eq!(found.character, 3);
}
