fn main() {
    match std::env::args().nth(1).as_deref() {
        None | Some("--stdio") => {}
        Some("--version" | "-V") => return println!("weft {}", env!("CARGO_PKG_VERSION")),
        Some(_) => {
            eprintln!("weft {}: a language server for Markdown notes.\nRun it from an editor; it speaks LSP over stdin and stdout.", env!("CARGO_PKG_VERSION"));
            std::process::exit(2);
        }
    }
    let (conn, io) = lsp_server::Connection::stdio();
    let result = weft::server::run(conn);
    io.join().ok();
    if let Err(e) = result {
        eprintln!("weft: {e}");
        std::process::exit(1);
    }
}
