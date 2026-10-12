//! The LSP transport (stdio, JSON-RPC via `lsp-server`) and request handlers.

// `Uri` caches its parse internally but hashes by its text, so it is a
// sound map key.
#![allow(clippy::mutable_key_type)]

use crate::analysis::{analyze_package, Analysis, Completion};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A line index for converting byte offsets to LSP positions (UTF-16).
struct Lines<'a> {
    src: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(src: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
        Lines { src, starts }
    }

    fn position(&self, off: usize) -> Position {
        let off = off.min(self.src.len());
        let line = self.starts.partition_point(|&s| s <= off) - 1;
        let col: usize = self.src[self.starts[line]..off]
            .chars()
            .map(char::len_utf16)
            .sum();
        Position::new(line as u32, col as u32)
    }

    fn offset(&self, p: Position) -> usize {
        let Some(&start) = self.starts.get(p.line as usize) else {
            return self.src.len();
        };
        let mut units = 0;
        for (i, c) in self.src[start..].char_indices() {
            if units >= p.character as usize || c == '\n' {
                return start + i;
            }
            units += c.len_utf16();
        }
        self.src.len()
    }

    fn range(&self, s: kiln_lexer::Span) -> Range {
        Range::new(self.position(s.start), self.position(s.end))
    }
}

/// An open document.
struct Doc {
    text: String,
    version: i32,
    /// The root file of the program it belongs to.
    root: PathBuf,
}

/// The analysis of one program (a package, or a single file).
struct Proj {
    analysis: Analysis,
    /// Members from the last analysis without errors (so `.` completion
    /// works while the line being typed doesn't parse).
    good_members: HashMap<String, Vec<Completion>>,
    /// The files diagnostics were last published for.
    published: Vec<PathBuf>,
}

struct State {
    docs: HashMap<Uri, Doc>,
    projs: HashMap<PathBuf, Proj>,
}

fn uri_to_path(u: &Uri) -> Option<PathBuf> {
    let s = u.as_str().strip_prefix("file://")?;
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    let p = String::from_utf8(out).ok()?;
    // `file:///C:/x` on Windows.
    let p = if p.len() > 2 && p.as_bytes()[2] == b':' {
        p[1..].to_string()
    } else {
        p
    };
    Some(PathBuf::from(p))
}

fn path_to_uri(p: &Path) -> Option<Uri> {
    let abs = p.canonicalize().unwrap_or(p.to_path_buf());
    let mut s = abs.to_string_lossy().replace('\\', "/");
    if !s.starts_with('/') {
        s.insert(0, '/');
    }
    let mut enc = String::from("file://");
    for c in s.chars() {
        match c {
            ' ' => enc.push_str("%20"),
            '#' => enc.push_str("%23"),
            '?' => enc.push_str("%3F"),
            '%' => enc.push_str("%25"),
            c => enc.push(c),
        }
    }
    enc.parse().ok()
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// The program a file belongs to: the root of its package (when the
/// package's module tree includes it), else the file itself. Returns the
/// root and the dependencies.
fn program_for(path: &Path) -> (PathBuf, Vec<kiln_parser::package::Dependency>) {
    let mut dir = path.parent();
    while let Some(d) = dir {
        let m = d.join("kiln.toml");
        if m.is_file() {
            let main = d.join("src").join("main.kn");
            let lib = d.join("src").join("lib.kn");
            let root = if main.is_file() { main } else { lib };
            return (
                root,
                kiln_parser::manifest::parse(&m)
                    .and_then(|m| kiln_parser::manifest::all_deps(&m))
                    .unwrap_or_default(),
            );
        }
        dir = d.parent();
    }
    (path.to_path_buf(), vec![])
}

impl State {
    /// Re-analyzes the program containing `uri`; returns its root.
    fn analyze(&mut self, uri: &Uri) -> Option<PathBuf> {
        let path = uri_to_path(uri)?;
        let (mut root, mut deps) = program_for(&path);
        let overlay = |p: &Path| -> Option<String> {
            self.docs
                .iter()
                .find(|(u, _)| uri_to_path(u).is_some_and(|q| same_file(&q, p)))
                .map(|(_, d)| d.text.clone())
        };
        let mut pkg = kiln_parser::package::load(&root, &deps, &overlay).ok();
        // A file outside its package's module tree is analyzed on its own.
        if !pkg
            .as_ref()
            .is_some_and(|p| p.files.iter().any(|(_, f, _)| same_file(f, &path)))
        {
            root = path.clone();
            deps = vec![];
            pkg = kiln_parser::package::load(&root, &deps, &overlay).ok();
        }
        let pkg = pkg?;
        // The front end may panic on input it doesn't handle yet; keep serving.
        let analysis =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| analyze_package(&pkg)))
                .unwrap_or_default();
        if let Some(d) = self.docs.get_mut(uri) {
            d.root = root.clone();
        }
        let ok = analysis.diagnostics.is_empty();
        let (good_members, published) = match self.projs.remove(&root) {
            Some(p) if !ok => (p.good_members, p.published),
            Some(p) => (analysis.members.clone(), p.published),
            None => (analysis.members.clone(), vec![]),
        };
        self.projs.insert(
            root.clone(),
            Proj {
                analysis,
                good_members,
                published,
            },
        );
        Some(root)
    }

    fn proj_of(&self, uri: &Uri) -> Option<(&Proj, usize)> {
        let doc = self.docs.get(uri)?;
        let p = self.projs.get(&doc.root)?;
        let path = uri_to_path(uri)?;
        let fi = p
            .analysis
            .files
            .iter()
            .position(|(_, f, _)| same_file(f, &path))?;
        Some((p, fi))
    }
}

/// A file's text inside the analysis, with its offset.
fn file_text(a: &Analysis, fi: usize) -> (usize, &str) {
    let (v, _, len) = &a.files[fi];
    (*v, &a.text[*v..*v + *len])
}

/// Converts an offset in the concatenated text to a location.
fn location(a: &Analysis, s: kiln_lexer::Span) -> Option<Location> {
    let fi = a
        .files
        .iter()
        .rposition(|(v, _, len)| *v <= s.start && s.start <= v + len)?;
    let (v, text) = file_text(a, fi);
    let lines = Lines::new(text);
    let local = kiln_lexer::Span::new(s.start - v, (s.end - v).min(text.len()));
    Some(Location::new(
        path_to_uri(&a.files[fi].1)?,
        lines.range(local),
    ))
}

/// Runs the server on stdin/stdout until the client shuts it down.
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (conn, io) = Connection::stdio();
    let caps = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        document_formatting_provider: Some(OneOf::Left(true)),
        inlay_hint_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".into(), ":".into()]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let init = serde_json::json!({
        "capabilities": caps,
        "serverInfo": { "name": "kiln-lsp", "version": env!("CARGO_PKG_VERSION") },
    });
    let (init_id, _params) = conn.initialize_start()?;
    conn.initialize_finish(init_id, init)?;
    let mut st = State {
        docs: HashMap::new(),
        projs: HashMap::new(),
    };
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn.handle_shutdown(&req)? {
                    break;
                }
                let resp = handle_request(&st, req);
                conn.sender.send(Message::Response(resp))?;
            }
            Message::Notification(n) => {
                if let Some(root) = handle_notification(&mut st, n) {
                    publish(&conn, &mut st, &root)?;
                }
            }
            Message::Response(_) => {}
        }
    }
    // The writer thread ends once the connection (its sender) is gone.
    drop(conn);
    io.join()?;
    Ok(())
}

fn publish(
    conn: &Connection,
    st: &mut State,
    root: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(p) = st.projs.get_mut(root) else {
        return Ok(());
    };
    let a = &p.analysis;
    let mut by_file: Vec<Vec<lsp_types::Diagnostic>> = vec![Vec::new(); a.files.len()];
    for d in &a.diagnostics {
        let fi = a
            .files
            .iter()
            .rposition(|(v, _, len)| *v <= d.span.start && d.span.start <= v + len)
            .unwrap_or(0);
        let (v, text) = file_text(a, fi);
        let lines = Lines::new(text);
        let local = kiln_lexer::Span::new(
            d.span.start.saturating_sub(v),
            d.span.end.saturating_sub(v).min(text.len()),
        );
        let mut message = d.message.clone();
        if let Some(h) = &d.help {
            message.push_str("\nhelp: ");
            message.push_str(h);
        }
        for note in &d.notes {
            message.push_str("\nnote: ");
            message.push_str(&note.0);
        }
        by_file[fi].push(lsp_types::Diagnostic {
            range: lines.range(local),
            severity: Some(match d.severity {
                kiln_lexer::Severity::Error => DiagnosticSeverity::ERROR,
                _ => DiagnosticSeverity::WARNING,
            }),
            source: Some("kiln".into()),
            message,
            ..Default::default()
        });
    }
    let mut now = Vec::new();
    for (fi, diags) in by_file.into_iter().enumerate() {
        let path = a.files[fi].1.clone();
        let Some(uri) = path_to_uri(&path) else {
            continue;
        };
        let version = st
            .docs
            .iter()
            .find(|(u, _)| uri_to_path(u).is_some_and(|q| same_file(&q, &path)))
            .map(|(_, d)| d.version);
        send_diags(conn, uri, diags, version)?;
        now.push(path);
    }
    // Clear files that are no longer part of the program.
    for old in std::mem::take(&mut p.published) {
        if !now.iter().any(|n| same_file(n, &old)) {
            if let Some(uri) = path_to_uri(&old) {
                send_diags(conn, uri, vec![], None)?;
            }
        }
    }
    p.published = now;
    Ok(())
}

fn send_diags(
    conn: &Connection,
    uri: Uri,
    diagnostics: Vec<lsp_types::Diagnostic>,
    version: Option<i32>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version,
    };
    conn.sender.send(Message::Notification(Notification::new(
        notification::PublishDiagnostics::METHOD.to_string(),
        params,
    )))?;
    Ok(())
}

fn handle_notification(st: &mut State, n: Notification) -> Option<PathBuf> {
    match n.method.as_str() {
        notification::DidOpenTextDocument::METHOD => {
            let p: DidOpenTextDocumentParams = serde_json::from_value(n.params).ok()?;
            let uri = p.text_document.uri.clone();
            st.docs.insert(
                uri.clone(),
                Doc {
                    text: p.text_document.text,
                    version: p.text_document.version,
                    root: PathBuf::new(),
                },
            );
            st.analyze(&uri)
        }
        notification::DidChangeTextDocument::METHOD => {
            let p: DidChangeTextDocumentParams = serde_json::from_value(n.params).ok()?;
            let text = p.content_changes.into_iter().last()?.text;
            let uri = p.text_document.uri.clone();
            let d = st.docs.get_mut(&uri)?;
            d.text = text;
            d.version = p.text_document.version;
            st.analyze(&uri)
        }
        notification::DidSaveTextDocument::METHOD => {
            let p: DidSaveTextDocumentParams = serde_json::from_value(n.params).ok()?;
            st.analyze(&p.text_document.uri)
        }
        notification::DidCloseTextDocument::METHOD => {
            let p: DidCloseTextDocumentParams = serde_json::from_value(n.params).ok()?;
            st.docs.remove(&p.text_document.uri);
            None
        }
        _ => None,
    }
}

fn ok<T: serde::Serialize>(id: RequestId, v: T) -> Response {
    Response::new_ok(id, v)
}

fn handle_request(st: &State, req: Request) -> Response {
    let id = req.id.clone();
    let none = Response::new_ok(id.clone(), serde_json::Value::Null);
    let uri_of = |v: &serde_json::Value| -> Option<Uri> {
        serde_json::from_value(v.get("textDocument")?.get("uri")?.clone()).ok()
    };
    let Some(uri) = uri_of(&req.params) else {
        return none;
    };
    let Some(doc) = st.docs.get(&uri) else {
        return none;
    };
    let Some((proj, fi)) = st.proj_of(&uri) else {
        return none;
    };
    let a = &proj.analysis;
    let (base, ftext) = file_text(a, fi);
    let lines = Lines::new(ftext);
    // Positions in this file become offsets in the analysis's text.
    let pos_of = |v: &serde_json::Value| -> Option<usize> {
        let p: Position = serde_json::from_value(v.get("position")?.clone()).ok()?;
        Some(base + lines.offset(p))
    };
    let local = |s: kiln_lexer::Span| {
        lines.range(kiln_lexer::Span::new(
            s.start.saturating_sub(base),
            s.end.saturating_sub(base).min(ftext.len()),
        ))
    };
    match req.method.as_str() {
        request::HoverRequest::METHOD => {
            let Some(at) = pos_of(&req.params) else {
                return none;
            };
            match a.hover(at) {
                Some((span, text)) => ok(
                    id,
                    Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: text,
                        }),
                        range: Some(local(span)),
                    },
                ),
                None => none,
            }
        }
        request::GotoDefinition::METHOD => {
            let Some(at) = pos_of(&req.params) else {
                return none;
            };
            match a.definition(at) {
                Some(s) => match location(a, s) {
                    Some(l) => ok(id, GotoDefinitionResponse::Scalar(l)),
                    None => none,
                },
                None => none,
            }
        }
        request::References::METHOD => {
            let Some(at) = pos_of(&req.params) else {
                return none;
            };
            let locs: Vec<Location> = a
                .references(at)
                .into_iter()
                .filter_map(|s| location(a, s))
                .collect();
            ok(id, locs)
        }
        request::PrepareRenameRequest::METHOD => {
            let Some(at) = pos_of(&req.params) else {
                return none;
            };
            let refs = a.references(at);
            match refs.iter().find(|s| s.start <= at && at <= s.end) {
                Some(s) => ok(id, PrepareRenameResponse::Range(local(*s))),
                None => none,
            }
        }
        request::Rename::METHOD => {
            let Some(at) = pos_of(&req.params) else {
                return none;
            };
            let new_name = req
                .params
                .get("newName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let valid = new_name
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
                && new_name.chars().all(|c| c.is_alphanumeric() || c == '_')
                && kiln_lexer::Keyword::lookup(&new_name).is_none();
            if !valid {
                return Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("`{new_name}` is not a valid name"),
                );
            }
            let refs = a.references(at);
            if refs.is_empty() {
                return Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    "nothing to rename here".into(),
                );
            }
            // Edits in every file of the program that mentions the name.
            let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
            for s in refs {
                if let Some(l) = location(a, s) {
                    changes
                        .entry(l.uri)
                        .or_default()
                        .push(TextEdit::new(l.range, new_name.clone()));
                }
            }
            ok(
                id,
                WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                },
            )
        }
        request::DocumentSymbolRequest::METHOD => {
            let end = base + ftext.len();
            #[allow(deprecated)]
            let syms: Vec<SymbolInformation> = a
                .symbols
                .iter()
                .filter(|s| s.span.start >= base && s.span.end <= end)
                .map(|s| SymbolInformation {
                    name: s.name.clone(),
                    kind: match s.kind {
                        "struct" => SymbolKind::STRUCT,
                        "enum" => SymbolKind::ENUM,
                        "variant" => SymbolKind::ENUM_MEMBER,
                        "trait" => SymbolKind::INTERFACE,
                        "method" => SymbolKind::METHOD,
                        "const" | "static" => SymbolKind::CONSTANT,
                        "type" => SymbolKind::TYPE_PARAMETER,
                        "mod" => SymbolKind::MODULE,
                        _ => SymbolKind::FUNCTION,
                    },
                    tags: None,
                    deprecated: None,
                    location: Location::new(uri.clone(), local(s.full)),
                    container_name: s.container.clone(),
                })
                .collect();
            ok(id, DocumentSymbolResponse::Flat(syms))
        }
        request::Formatting::METHOD => match kiln_fmt::format(&doc.text) {
            Ok(out) if out != doc.text => {
                let end = Lines::new(&doc.text).position(doc.text.len());
                ok(
                    id,
                    vec![TextEdit::new(Range::new(Position::new(0, 0), end), out)],
                )
            }
            _ => ok(id, Vec::<TextEdit>::new()),
        },
        request::InlayHintRequest::METHOD => {
            let end = base + ftext.len();
            let hints: Vec<InlayHint> = a
                .inlay_hints()
                .into_iter()
                .filter(|(off, _)| *off >= base && *off <= end)
                .map(|(off, label)| InlayHint {
                    position: lines.position(off - base),
                    label: InlayHintLabel::String(label),
                    kind: Some(InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: None,
                    data: None,
                })
                .collect();
            ok(id, hints)
        }
        request::Completion::METHOD => {
            let Some(at) = pos_of(&req.params) else {
                return none;
            };
            // Complete against the live text (the analysis may be older).
            let live_at = at - base;
            let mut items = a.completions(&a.text, at);
            if items.is_empty() {
                items = member_fallback(proj, &doc.text, live_at);
            }
            let items: Vec<CompletionItem> = items
                .into_iter()
                .map(|c| CompletionItem {
                    label: c.label,
                    kind: Some(match c.kind {
                        "method" => CompletionItemKind::METHOD,
                        "field" => CompletionItemKind::FIELD,
                        "local" => CompletionItemKind::VARIABLE,
                        "fun" => CompletionItemKind::FUNCTION,
                        "struct" => CompletionItemKind::STRUCT,
                        "enum" => CompletionItemKind::ENUM,
                        "trait" => CompletionItemKind::INTERFACE,
                        "keyword" => CompletionItemKind::KEYWORD,
                        "const" | "static" => CompletionItemKind::CONSTANT,
                        "variant" => CompletionItemKind::ENUM_MEMBER,
                        _ => CompletionItemKind::MODULE,
                    }),
                    detail: if c.detail.is_empty() {
                        None
                    } else {
                        Some(c.detail)
                    },
                    ..Default::default()
                })
                .collect();
            ok(id, CompletionResponse::Array(items))
        }
        _ => Response::new_err(
            id,
            lsp_server::ErrorCode::MethodNotFound as i32,
            format!("unsupported: {}", req.method),
        ),
    }
}

/// `.` completion from the last analysis without errors: the type of the
/// local named just before the dot.
fn member_fallback(p: &Proj, text: &str, at: usize) -> Vec<Completion> {
    let before = &text[..at.min(text.len())];
    let word_start = before
        .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
        .len();
    let prefix = &before[word_start..];
    if !before[..word_start].ends_with('.') {
        return vec![];
    }
    let recv = &before[..word_start - 1];
    let name_start = recv
        .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
        .len();
    let name = &recv[name_start..];
    if name.is_empty() {
        return vec![];
    }
    let ty = p
        .analysis
        .locals
        .values()
        .find(|(n, _)| n == name)
        .map(|(_, t)| {
            t.trim_start_matches('&')
                .trim_start_matches("mut ")
                .split('<')
                .next()
                .unwrap_or("")
                .to_string()
        });
    let Some(ty) = ty else { return vec![] };
    let mut v: Vec<Completion> = p.good_members.get(&ty).cloned().unwrap_or_default();
    v.retain(|c| c.label.starts_with(prefix));
    v
}
