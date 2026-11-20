const HELP: &str = "usage: weft [lsp]                  run the language server (from an editor)
       weft changes <since> <until>  list the notes changed in git history
       weft --version";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("lsp" | "--stdio") => {}
        Some("--version" | "-V") => return println!("weft {}", env!("CARGO_PKG_VERSION")),
        Some("changes") => {
            if let Err(e) = weft::changes::main(&args[1..]) {
                eprintln!("{e}");
                std::process::exit(2);
            }
            return;
        }
        Some(_) => {
            eprintln!("weft {}: a language server for Markdown notes.\n\n{HELP}", env!("CARGO_PKG_VERSION"));
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
