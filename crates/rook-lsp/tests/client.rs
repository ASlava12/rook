use std::path::PathBuf;

use rook_lsp::{LspError, Server, ServerConfig};

fn mock() -> ServerConfig {
    ServerConfig {
        language: "mock".into(),
        command: env!("CARGO_BIN_EXE_lsp-mock").to_string(),
        extensions: vec!["rs".into()],
        startup_timeout_secs: 10,
        request_timeout_secs: 5,
        diagnostics_wait_ms: 2_000,
        ..Default::default()
    }
}

struct Workspace {
    dir: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn parse(input: &str) -> usize {\n    input.len()\n}\n\nfn broken() { oops }\n",
        )
        .unwrap();
        Self { dir }
    }

    fn file(&self) -> PathBuf {
        self.dir.path().join("src/lib.rs")
    }
}

#[tokio::test]
async fn the_handshake_completes_over_content_length_framing() {
    let workspace = Workspace::new();
    let server = Server::start(&mock(), workspace.dir.path()).await.unwrap();
    assert_eq!(server.language(), "mock");
    server.shutdown().await;
}

#[tokio::test]
async fn diagnostics_arrive_after_a_file_is_opened() {
    let workspace = Workspace::new();
    let server = Server::start(&mock(), workspace.dir.path()).await.unwrap();
    server.sync(&workspace.file(), "rust").await.unwrap();

    let found = server.diagnostics(&workspace.file()).await;
    assert_eq!(found.len(), 1, "the server publishes one; none means didOpen never landed");
    assert_eq!(found[0].severity_name(), "error");
    assert!(found[0].message.contains("oops"));
    server.shutdown().await;
}

#[tokio::test]
async fn a_definition_answered_as_a_single_object_is_understood() {
    let workspace = Workspace::new();
    let server = Server::start(&mock(), workspace.dir.path()).await.unwrap();
    server.sync(&workspace.file(), "rust").await.unwrap();

    let found =
        server.definition(&workspace.file(), rook_lsp::Position { line: 1, character: 4 }).await.unwrap();
    assert_eq!(found.len(), 1, "the spec allows a bare object as well as a list");
    // LSP counts from zero; a person and a model count from one.
    assert_eq!(found[0].line, 11);
    assert_eq!(found[0].character, 4);
    assert!(!found[0].path.starts_with("file://"), "a uri in a tool result is noise: {}", found[0].path);
    server.shutdown().await;
}

#[tokio::test]
async fn references_come_back_as_a_list() {
    let workspace = Workspace::new();
    let server = Server::start(&mock(), workspace.dir.path()).await.unwrap();
    server.sync(&workspace.file(), "rust").await.unwrap();

    let found =
        server.references(&workspace.file(), rook_lsp::Position { line: 0, character: 7 }).await.unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].line, 2);
    assert_eq!(found[1].line, 21);
    server.shutdown().await;
}

#[tokio::test]
async fn workspace_symbols_carry_a_readable_kind() {
    let workspace = Workspace::new();
    let server = Server::start(&mock(), workspace.dir.path()).await.unwrap();
    let found = server.symbols("parse").await.unwrap();
    assert_eq!(found[0].name, "parse");
    assert_eq!(found[0].kind, "function", "a bare LSP kind number tells the model nothing");
    assert_eq!(found[0].container.as_deref(), Some("app"));
    assert_eq!(found[0].line, 42);
    server.shutdown().await;
}

#[tokio::test]
async fn a_missing_language_server_names_the_command() {
    let workspace = Workspace::new();
    let config = ServerConfig { command: "no-such-language-server".into(), ..mock() };
    let Err(err) = Server::start(&config, workspace.dir.path()).await else {
        panic!("it should not have started")
    };
    assert!(matches!(err, LspError::Spawn { .. }));
    assert!(err.to_string().contains("no-such-language-server"), "{err}");
}

#[tokio::test]
async fn extensions_decide_which_server_handles_a_file() {
    let config = mock();
    assert!(config.handles(std::path::Path::new("src/lib.rs")));
    assert!(config.handles(std::path::Path::new("SRC/LIB.RS")), "extensions are case-insensitive");
    assert!(!config.handles(std::path::Path::new("README.md")));
    assert!(!config.handles(std::path::Path::new("Makefile")));
}

#[test]
fn a_symbol_is_located_by_name_on_a_word_boundary() {
    let text = "fn parse_all() {}\nfn parse() {}\nlet x = parse();\n";
    let at = rook_lsp::locate(text, "parse").expect("the name is there");
    assert_eq!(at.line, 1, "parse_all must not match: it is a different name");
    assert_eq!(at.character, 3);
    assert!(rook_lsp::locate(text, "missing").is_none());
}

#[tokio::test]
async fn an_edited_file_is_re_analysed_rather_than_answered_from_the_old_version() {
    let workspace = Workspace::new();
    let server = Server::start(&mock(), workspace.dir.path()).await.unwrap();
    server.sync(&workspace.file(), "rust").await.unwrap();

    let before = server.diagnostics(&workspace.file()).await;
    assert!(before[0].message.contains("5 lines seen"), "{}", before[0].message);

    std::fs::write(
        workspace.file(),
        "// a new first line\n// and another\npub fn parse(input: &str) -> usize {\n    input.len()\n}\n\nfn broken() { oops }\n",
    )
    .unwrap();
    server.sync(&workspace.file(), "rust").await.unwrap();

    let after = server.diagnostics(&workspace.file()).await;
    assert!(
        after[0].message.contains("7 lines seen"),
        "the server answered from the version it saw at open time: {}",
        after[0].message
    );
    server.shutdown().await;
}

#[tokio::test]
async fn syncing_an_unchanged_file_costs_nothing() {
    let workspace = Workspace::new();
    let server = Server::start(&mock(), workspace.dir.path()).await.unwrap();
    server.sync(&workspace.file(), "rust").await.unwrap();
    let first = server.diagnostics(&workspace.file()).await;

    // No edit: the second sync must not discard the analysis and ask again. The
    // server counts what it was asked to analyse, so this is what happened
    // rather than how long it took — a timing bound measures the machine.
    server.sync(&workspace.file(), "rust").await.unwrap();
    let second = server.diagnostics(&workspace.file()).await;

    assert_eq!(first[0].message, second[0].message);
    assert!(second[0].message.contains("analysis 1"), "it re-analysed anyway: {}", second[0].message);
    server.shutdown().await;
}
