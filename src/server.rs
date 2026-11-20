//! The protocol loop. One thread owns all state and handles messages in
//! order; the watcher's events arrive on a channel and are coalesced.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _, PublishDiagnostics,
};
use lsp_types::request::{
    Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, PrepareRenameRequest, References, Rename,
    Request as _, WillRenameFiles, WorkspaceSymbolRequest,
};
use lsp_types::*;
use notify::Watcher;
use serde_json::Value;

use crate::ctx::Ctx;
use crate::text::{self, Encoding};
use crate::vault::{self, Root, Vault};
use crate::{complete, diagnose, hover, refs, rename, symbols};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// How long to wait for a burst of file events (a pull) to end.
const SETTLE: Duration = Duration::from_millis(100);
/// How long the `willRenameFiles` echo of our own rename is expected.
const ECHO: Duration = Duration::from_secs(10);

macro_rules! log {
    ($($t:tt)*) => {
        if std::env::var_os("WEFT_LOG").is_some() {
            eprintln!("weft: {}", format!($($t)*));
        }
    };
}

pub struct Server {
    conn: Connection,
    enc: Encoding,
    vaults: Vec<Vault>,
    watcher: Option<notify::RecommendedWatcher>,
    events_tx: Sender<PathBuf>,
    events: Receiver<PathBuf>,
    /// Vaults with file events waiting, and when to look at them.
    pending: HashSet<usize>,
    deadline: Option<Instant>,
    /// The file move in the rename we last answered.
    echo: Option<(Url, Url, Instant)>,
}

pub fn capabilities(enc: Encoding) -> ServerCapabilities {
    let all = FileOperationRegistrationOptions {
        filters: vec![FileOperationFilter {
            scheme: Some("file".into()),
            pattern: FileOperationPattern { glob: "**".into(), matches: None, options: None },
        }],
    };
    ServerCapabilities {
        position_encoding: Some(match enc {
            Encoding::Utf8 => PositionEncodingKind::UTF8,
            Encoding::Utf16 => PositionEncodingKind::UTF16,
        }),
        text_document_sync: Some(TextDocumentSyncCapability::Options(TextDocumentSyncOptions {
            open_close: Some(true),
            change: Some(TextDocumentSyncKind::INCREMENTAL),
            ..Default::default()
        })),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(complete::TRIGGERS.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: None,
            file_operations: Some(WorkspaceFileOperationsServerCapabilities {
                will_rename: Some(all),
                ..Default::default()
            }),
        }),
        ..Default::default()
    }
}

fn negotiate(params: &InitializeParams) -> Encoding {
    let offered = params.capabilities.general.as_ref().and_then(|g| g.position_encodings.as_ref());
    if offered.is_some_and(|o| o.contains(&PositionEncodingKind::UTF8)) { Encoding::Utf8 } else { Encoding::Utf16 }
}

pub fn run(conn: Connection) -> Result<(), Error> {
    let (id, params) = conn.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(params)?;
    let enc = negotiate(&params);
    let result = InitializeResult {
        capabilities: capabilities(enc),
        server_info: Some(ServerInfo { name: "weft".into(), version: Some(env!("CARGO_PKG_VERSION").into()) }),
    };
    conn.initialize_finish(id, serde_json::to_value(result)?)?;
    log!("initialized, encoding {enc:?}");

    let (events_tx, events) = crossbeam_channel::unbounded();
    let mut server = Server {
        conn,
        enc,
        vaults: Vec::new(),
        watcher: None,
        events_tx,
        events,
        pending: HashSet::new(),
        deadline: None,
        echo: None,
    };
    server.watcher = server.start_watcher();
    server.main_loop()
}

impl Server {
    fn start_watcher(&self) -> Option<notify::RecommendedWatcher> {
        let tx = self.events_tx.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                if event.kind.is_access() {
                    return;
                }
                for path in event.paths {
                    let _ = tx.send(path);
                }
            }
        });
        match watcher {
            Ok(w) => Some(w),
            Err(e) => {
                log!("no file watcher: {e}");
                None
            }
        }
    }

    fn main_loop(&mut self) -> Result<(), Error> {
        loop {
            let wait = self.deadline.map_or(Duration::from_secs(3600), |d| d.saturating_duration_since(Instant::now()));
            select! {
                recv(self.conn.receiver) -> msg => {
                    let Ok(msg) = msg else { return Ok(()) };
                    match msg {
                        Message::Request(req) => {
                            if self.conn.handle_shutdown(&req)? {
                                return Ok(());
                            }
                            self.request(req);
                        }
                        Message::Notification(n) => self.notification(n),
                        Message::Response(_) => {}
                    }
                }
                recv(self.events) -> path => {
                    if let Ok(path) = path {
                        self.file_event(&path);
                    }
                }
                default(wait) => {}
            }
            if self.deadline.is_some_and(|d| Instant::now() >= d) {
                self.settle();
            }
        }
    }

    // --- Vaults ---------------------------------------------------------

    /// The vault and path from its root of a document, loading the vault on
    /// first sight.
    fn locate(&mut self, uri: &Url) -> Option<(usize, String)> {
        let path = uri.to_file_path().ok()?;
        let found = self
            .vaults
            .iter()
            .enumerate()
            .filter(|(_, v)| v.contains(&path))
            .max_by_key(|(_, v)| v.root.components().count());
        if let Some((i, v)) = found {
            return Some((i, v.rel(&path)?));
        }
        let (root, kind) = vault::discover(&path);
        let start = Instant::now();
        let only = path.strip_prefix(&root).ok().map(|p| p.to_string_lossy().into_owned());
        let vault = Vault::load(root.clone(), kind, only.as_deref());
        log!("loaded {} ({:?}, {} files) in {:?}", root.display(), kind, vault.files.len(), start.elapsed());
        if kind != Root::Single {
            if let Some(w) = self.watcher.as_mut() {
                if let Err(e) = w.watch(&root, notify::RecursiveMode::Recursive) {
                    log!("cannot watch {}: {e}", root.display());
                }
            }
        }
        let rel = vault.rel(&path)?;
        self.vaults.push(vault);
        Some((self.vaults.len() - 1, rel))
    }

    fn file_event(&mut self, path: &Path) {
        for (i, v) in self.vaults.iter().enumerate() {
            let Some(rel) = v.rel(path) else { continue };
            if rel.split('/').any(|p| p.starts_with('.')) {
                continue;
            }
            if v.kind == Root::Repo && !vault::is_note(&rel) && path.extension().is_some() {
                continue;
            }
            self.pending.insert(i);
            self.deadline = Some(Instant::now() + SETTLE);
        }
    }

    /// The burst of file events is over: bring the vaults in line with disk.
    fn settle(&mut self) {
        self.deadline = None;
        for i in std::mem::take(&mut self.pending) {
            let start = Instant::now();
            if self.vaults[i].rescan() {
                log!("rescanned {} in {:?}", self.vaults[i].root.display(), start.elapsed());
                self.publish_all(i);
            }
        }
    }

    // --- Messages -------------------------------------------------------

    fn notification(&mut self, n: Notification) {
        match n.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(n.params) else { return };
                let Some((i, rel)) = self.locate(&p.text_document.uri) else { return };
                self.vaults[i].open(&rel, p.text_document.text, p.text_document.version);
                self.publish_all(i);
            }
            DidChangeTextDocument::METHOD => {
                let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(n.params) else { return };
                let Some((i, rel)) = self.locate(&p.text_document.uri) else { return };
                let enc = self.enc;
                let Some(file) = self.vaults[i].file_mut(&rel) else { return };
                let Some(text) = file.text.as_mut() else { return };
                for change in p.content_changes {
                    text::apply_change(text, change, enc);
                }
                file.open = Some(p.text_document.version);
                self.vaults[i].reparse(&rel);
                self.publish_all(i);
            }
            DidCloseTextDocument::METHOD => {
                let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(n.params) else { return };
                let Some((i, rel)) = self.locate(&p.text_document.uri) else { return };
                self.vaults[i].close(&rel);
                self.send_diagnostics(p.text_document.uri, Vec::new(), None);
                self.publish_all(i);
            }
            _ => {}
        }
    }

    fn request(&mut self, req: Request) {
        if req.method != WillRenameFiles::METHOD {
            self.echo = None;
        }
        let id = req.id.clone();
        let result = self.dispatch(req);
        let response = match result {
            Ok(value) => Response { id, result: Some(value), error: None },
            Err((code, message)) => Response::new_err(id, code as i32, message),
        };
        let _ = self.conn.sender.send(Message::Response(response));
    }

    fn dispatch(&mut self, req: Request) -> Result<Value, (ErrorCode, String)> {
        let bad = |e: serde_json::Error| (ErrorCode::InvalidParams, e.to_string());
        let null = Ok(Value::Null);
        macro_rules! params {
            ($t:ty) => {
                serde_json::from_value::<$t>(req.params).map_err(bad)?
            };
        }
        macro_rules! at {
            ($p:expr) => {{
                let Some((i, rel)) = self.locate(&$p.text_document.uri) else { return null };
                (Ctx { vault: &self.vaults[i], enc: self.enc }, rel)
            }};
        }
        let json = |v: serde_json::Result<Value>| v.map_err(|e| (ErrorCode::InternalError, e.to_string()));
        match req.method.as_str() {
            HoverRequest::METHOD => {
                let p = params!(HoverParams);
                let pos = p.text_document_position_params.position;
                let (cx, rel) = at!(p.text_document_position_params);
                json(serde_json::to_value(hover::hover(&cx, &rel, pos)))
            }
            GotoDefinition::METHOD => {
                let p = params!(GotoDefinitionParams);
                let pos = p.text_document_position_params.position;
                let (cx, rel) = at!(p.text_document_position_params);
                json(serde_json::to_value(refs::definition(&cx, &rel, pos).map(GotoDefinitionResponse::Scalar)))
            }
            References::METHOD => {
                let p = params!(ReferenceParams);
                let pos = p.text_document_position.position;
                let decl = p.context.include_declaration;
                let (cx, rel) = at!(p.text_document_position);
                json(serde_json::to_value(refs::references(&cx, &rel, pos, decl)))
            }
            Completion::METHOD => {
                let p = params!(CompletionParams);
                let pos = p.text_document_position.position;
                let (cx, rel) = at!(p.text_document_position);
                json(serde_json::to_value(complete::complete(&cx, &rel, pos).map(CompletionResponse::List)))
            }
            PrepareRenameRequest::METHOD => {
                let p = params!(TextDocumentPositionParams);
                let pos = p.position;
                let (cx, rel) = at!(p);
                let r = rename::prepare(&cx, &rel, pos).map_err(|m| (ErrorCode::RequestFailed, m))?;
                json(serde_json::to_value(r))
            }
            Rename::METHOD => {
                let p = params!(RenameParams);
                // Edits to closed notes must match the disk now, not as of
                // the last file event.
                self.refresh(&p.text_document_position.text_document.uri);
                let pos = p.text_document_position.position;
                let (cx, rel) = at!(p.text_document_position);
                let (edit, moved) =
                    rename::rename(&cx, &rel, pos, &p.new_name).map_err(|m| (ErrorCode::RequestFailed, m))?;
                let echo = moved.map(|(a, b)| (cx.uri(&a), cx.uri(&b), Instant::now()));
                self.echo = echo;
                json(serde_json::to_value(edit))
            }
            DocumentSymbolRequest::METHOD => {
                let p = params!(DocumentSymbolParams);
                let (cx, rel) = at!(p);
                json(serde_json::to_value(DocumentSymbolResponse::Nested(symbols::document(&cx, &rel))))
            }
            WorkspaceSymbolRequest::METHOD => {
                let p = params!(WorkspaceSymbolParams);
                let mut all = Vec::new();
                for v in &self.vaults {
                    all.extend(symbols::workspace(&Ctx { vault: v, enc: self.enc }, &p.query));
                }
                json(serde_json::to_value(all))
            }
            WillRenameFiles::METHOD => {
                let p = params!(RenameFilesParams);
                let files: Vec<(Url, Url)> = p
                    .files
                    .iter()
                    .filter_map(|f| Some((Url::parse(&f.old_uri).ok()?, Url::parse(&f.new_uri).ok()?)))
                    .collect();
                // Our own rename, coming back from Helix as it moves the file.
                if let Some((a, b, at)) = self.echo.take() {
                    if at.elapsed() < ECHO && files.len() == 1 && same(&files[0].0, &a) && same(&files[0].1, &b) {
                        log!("ignoring the echo of our rename");
                        return null;
                    }
                }
                let Some((old, _)) = files.first() else { return null };
                let Some(i) = self.vault_of(old) else { return null };
                self.vaults[i].rescan();
                let cx = Ctx { vault: &self.vaults[i], enc: self.enc };
                json(serde_json::to_value(rename::will_rename(&cx, &files)))
            }
            _ => Err((ErrorCode::MethodNotFound, format!("weft does not handle {}", req.method))),
        }
    }

    /// Rescans the vault of a document now, without waiting for its file
    /// events to settle.
    fn refresh(&mut self, uri: &Url) {
        if let Some((i, _)) = self.locate(uri) {
            if self.vaults[i].rescan() {
                self.publish_all(i);
            }
        }
    }

    /// The vault containing a path, which may be a folder.
    fn vault_of(&self, uri: &Url) -> Option<usize> {
        let path = uri.to_file_path().ok()?;
        self.vaults
            .iter()
            .enumerate()
            .filter(|(_, v)| v.kind != Root::Single && path.starts_with(&v.root))
            .max_by_key(|(_, v)| v.root.components().count())
            .map(|(i, _)| i)
    }

    // --- Diagnostics ----------------------------------------------------

    /// Diagnostics for every open note in a vault: a change in one note can
    /// fix or break links in another.
    fn publish_all(&self, i: usize) {
        let v = &self.vaults[i];
        let cx = Ctx { vault: v, enc: self.enc };
        for (rel, file) in &v.files {
            if let Some(version) = file.open {
                self.send_diagnostics(cx.uri(rel), diagnose::diagnose(&cx, rel), Some(version));
            }
        }
    }

    fn send_diagnostics(&self, uri: Url, diagnostics: Vec<Diagnostic>, version: Option<i32>) {
        let params = PublishDiagnosticsParams { uri, diagnostics, version };
        let n = Notification::new(PublishDiagnostics::METHOD.into(), params);
        let _ = self.conn.sender.send(Message::Notification(n));
    }
}

/// URIs equal as paths: percent-encoding may differ.
fn same(a: &Url, b: &Url) -> bool {
    match (a.to_file_path(), b.to_file_path()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
