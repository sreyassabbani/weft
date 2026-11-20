//! The server driven through an in-memory connection, over a vault in a
//! temporary folder.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::{Value, json};

struct Client {
    conn: Connection,
    next: i32,
    notifications: Vec<Notification>,
    root: PathBuf,
}

impl Client {
    fn start(files: &[(&str, &str)]) -> Client {
        let root = std::env::temp_dir()
            .join(format!("weft-test-{}-{}", std::process::id(), rand_suffix()))
            .canonicalize_parent();
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        for (path, text) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let (server, conn) = Connection::memory();
        std::thread::spawn(move || weft::server::run(server).unwrap());
        let mut client = Client { conn, next: 0, notifications: Vec::new(), root };
        let caps = json!({ "general": { "positionEncodings": ["utf-8", "utf-16"] } });
        let init = client.request("initialize", json!({ "capabilities": caps, "processId": null, "rootUri": null }));
        assert_eq!(init["capabilities"]["positionEncoding"], "utf-8");
        client.notify("initialized", json!({}));
        client
    }

    fn uri(&self, rel: &str) -> String {
        url_of(&self.root.join(rel))
    }

    fn open(&mut self, rel: &str) {
        let text = std::fs::read_to_string(self.root.join(rel)).unwrap_or_default();
        let uri = self.uri(rel);
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "markdown", "version": 1, "text": text } }),
        );
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.conn.sender.send(Message::Notification(Notification::new(method.into(), params))).unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = RequestId::from(self.next);
        self.conn.sender.send(Message::Request(Request::new(id.clone(), method.into(), params))).unwrap();
        loop {
            match self.conn.receiver.recv_timeout(Duration::from_secs(10)).expect("no response") {
                Message::Response(r) if r.id == id => {
                    if let Some(e) = r.error {
                        return json!({ "error": e.message });
                    }
                    return r.result.unwrap_or(Value::Null);
                }
                Message::Notification(n) => self.notifications.push(n),
                _ => {}
            }
        }
    }

    fn at(&self, rel: &str, line: u32, character: u32) -> Value {
        json!({ "textDocument": { "uri": self.uri(rel) }, "position": { "line": line, "character": character } })
    }

    /// Waits for diagnostics for `rel` matching `want`.
    fn diagnostics_until(&mut self, rel: &str, want: impl Fn(&Value) -> bool) -> bool {
        let uri = self.uri(rel);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let found = self.notifications.drain(..).any(|n| {
                n.method == "textDocument/publishDiagnostics" && n.params["uri"] == uri.as_str() && want(&n.params["diagnostics"])
            });
            if found {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.conn.receiver.recv_timeout(left) {
                Ok(Message::Notification(n)) => self.notifications.push(n),
                Ok(_) => {}
                Err(_) => return false,
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

trait CanonicalParent {
    fn canonicalize_parent(self) -> PathBuf;
}

impl CanonicalParent for PathBuf {
    /// The temporary folder is behind a symlink on macOS; the watcher
    /// reports real paths.
    fn canonicalize_parent(self) -> PathBuf {
        let parent = self.parent().unwrap().canonicalize().unwrap();
        parent.join(self.file_name().unwrap())
    }
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

fn url_of(path: &Path) -> String {
    lsp_types::Url::from_file_path(path).unwrap().to_string()
}

const VAULT: &[(&str, &str)] = &[
    ("Array.md", "# Array\n## C++\n### Dynamic Allocation\nvectors grow\n"),
    (
        "Sorting.md",
        "To sort [[Array#C++#Dynamic Allocation|vectors]] see [[Array]].\n[[Dynamic Call]] [[Other#Missing]] ![[gone.png]]\n",
    ),
    ("Can combinatorics be explained entirely physically?.md", "# Enumeration\n"),
    ("2026/11/14.md", "[[Sorting]]\n"),
    ("2026/10/14.md", ""),
    ("Other.md", "# Other\n"),
];

#[test]
fn navigation() {
    let mut c = Client::start(VAULT);
    c.open("Sorting.md");

    // Definition on a heading link goes to the heading.
    let def = c.request("textDocument/definition", c.at("Sorting.md", 0, 30));
    assert_eq!(def["uri"], c.uri("Array.md"));
    assert_eq!(def["range"]["start"], json!({ "line": 2, "character": 4 }));
    // On a note link, to the note.
    let def = c.request("textDocument/definition", c.at("Sorting.md", 0, 56));
    assert_eq!(def["range"]["start"], json!({ "line": 0, "character": 0 }));
    // On a note not written yet, to where it would be created.
    let def = c.request("textDocument/definition", c.at("Sorting.md", 1, 4));
    assert_eq!(def["uri"], c.uri("Dynamic Call.md"));

    let refs = c.request(
        "textDocument/references",
        json!({ "textDocument": { "uri": c.uri("Sorting.md") }, "position": { "line": 0, "character": 56 }, "context": { "includeDeclaration": false } }),
    );
    assert_eq!(refs.as_array().unwrap().len(), 2);
    assert_eq!(refs[0]["range"], json!({ "start": { "line": 0, "character": 8 }, "end": { "line": 0, "character": 48 } }));

    let hover = c.request("textDocument/hover", c.at("Sorting.md", 1, 4));
    assert_eq!(hover["contents"]["value"], "Not written yet · linked from 1 note");
    let hover = c.request("textDocument/hover", c.at("Sorting.md", 0, 56));
    let text = hover["contents"]["value"].as_str().unwrap();
    assert!(text.starts_with("**Array** · linked from 1 note"), "{text}");

    let symbols = c.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": c.uri("Sorting.md") } }));
    assert_eq!(symbols, json!([]));
    let symbols = c.request("workspace/symbol", json!({ "query": "dynamic" }));
    assert_eq!(symbols[0]["name"], "Dynamic Allocation");
}

#[test]
fn completion_on_multi_word_titles() {
    let mut c = Client::start(VAULT);
    c.open("2026/11/14.md");
    let edit = |c: &mut Client, text: &str| {
        let uri = c.uri("2026/11/14.md");
        c.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [{ "text": text }] }),
        );
    };
    edit(&mut c, "[[can combinatorics be ex");
    let list = c.request("textDocument/completion", c.at("2026/11/14.md", 0, 25));
    assert_eq!(list["isIncomplete"], true);
    let first = &list["items"][0];
    assert_eq!(first["textEdit"]["newText"], "Can combinatorics be explained entirely physically?]]");
    // Daily notes are written with their full path: `14` is not unique.
    edit(&mut c, "[[11/14");
    let list = c.request("textDocument/completion", c.at("2026/11/14.md", 0, 7));
    assert_eq!(list["items"][0]["textEdit"]["newText"], "2026/11/14]]");
    // Outside a link there is nothing.
    edit(&mut c, "plain text ");
    let list = c.request("textDocument/completion", c.at("2026/11/14.md", 0, 11));
    assert_eq!(list, Value::Null);
}

#[test]
fn diagnostics_follow_the_disk() {
    let mut c = Client::start(VAULT);
    c.open("Sorting.md");
    let messages = |d: &Value| d.as_array().unwrap().iter().map(|d| d["message"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert!(c.diagnostics_until("Sorting.md", |d| {
        messages(d) == ["No heading “Missing” in “Other”", "No file “gone.png” in the vault"]
    }));
    // Something else adds the heading and the file.
    std::fs::write(c.root.join("Other.md"), "# Other\n## Missing\n").unwrap();
    std::fs::write(c.root.join("gone.png"), "").unwrap();
    assert!(c.diagnostics_until("Sorting.md", |d| d.as_array().unwrap().is_empty()), "diagnostics not cleared");
}

#[test]
fn rename_and_echo() {
    let mut c = Client::start(VAULT);
    c.open("Sorting.md");
    let prep = c.request("textDocument/prepareRename", c.at("Sorting.md", 0, 56));
    assert_eq!(prep["placeholder"], "Array");
    let mut params = c.at("Sorting.md", 0, 56);
    params["newName"] = json!("Arrays");
    let edit = c.request("textDocument/rename", params);
    let ops = edit["documentChanges"].as_array().unwrap();
    let last = ops.last().unwrap();
    assert_eq!(last["kind"], "rename");
    assert_eq!(last["newUri"], c.uri("Arrays.md"));
    // The open note's edit carries its version.
    assert_eq!(ops[0]["textDocument"]["version"], 1);

    // Helix asks again while applying the move: answered with nothing.
    let files = json!({ "files": [{ "oldUri": c.uri("Array.md"), "newUri": c.uri("Arrays.md") }] });
    assert_eq!(c.request("workspace/willRenameFiles", files.clone()), Value::Null);
    // A real `:move` later is answered.
    let edit = c.request("workspace/willRenameFiles", files);
    assert_eq!(edit["documentChanges"].as_array().unwrap().len(), 1);

    // Renaming onto an existing note is refused.
    let mut params = c.at("Sorting.md", 0, 56);
    params["newName"] = json!("Other");
    let err = c.request("textDocument/rename", params);
    assert!(err["error"].as_str().unwrap().contains("already exists"));
}

#[test]
fn rename_sees_the_disk_as_it_is_now() {
    let mut c = Client::start(VAULT);
    c.open("Sorting.md");
    // A pull rewrites a closed note that links to `Array`, and a rename
    // comes before the file events settle.
    std::fs::write(c.root.join("Pulled.md"), "new first line\n\u{feff}x\n[[Array]]\n").unwrap();
    std::fs::write(c.root.join("Bom.md"), "\u{feff}[[Array]] é\n").unwrap();
    let mut params = c.at("Sorting.md", 0, 56);
    params["newName"] = json!("Arrays");
    let edit = c.request("textDocument/rename", params);
    let ops = edit["documentChanges"].as_array().unwrap();
    let range_in = |name: &str| {
        let uri = c.uri(name);
        ops.iter().find(|o| o["textDocument"]["uri"] == uri.as_str()).map(|o| o["edits"][0]["range"].clone())
    };
    assert_eq!(range_in("Pulled.md").unwrap()["start"], json!({ "line": 2, "character": 2 }));
    // Helix drops the byte order mark when it loads the file.
    assert_eq!(range_in("Bom.md").unwrap(), json!({ "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 7 } }));
}
