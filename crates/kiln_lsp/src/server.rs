//! The LSP transport (stdio, JSON-RPC via `lsp-server`) and request handlers.

// `Uri` caches its parse internally but hashes by its text, so it is a
// sound map key.
#![allow(clippy::mutable_key_type)]

use crate::analysis::{analyze, Analysis, Completion};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::*;
use std::collections::HashMap;


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

struct Doc {
    text: String,
    version: i32,
    analysis: Analysis,
    /// Members from the last analysis without errors (so `.` completion
    /// works while the line being typed doesn't parse).
    good_members: HashMap<String, Vec<Completion>>,
    good_type_names: Vec<(kiln_lexer::Span, String)>,
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
    let mut docs: HashMap<Uri, Doc> = HashMap::new();
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn.handle_shutdown(&req)? {
                    break;
                }
                let resp = handle_request(&docs, req);
                conn.sender.send(Message::Response(resp))?;
            }
            Message::Notification(n) => {
                if let Some(uri) = handle_notification(&mut docs, n) {
                    publish(&conn, &docs, &uri)?;
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
    docs: &HashMap<Uri, Doc>,
    uri: &Uri,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(doc) = docs.get(uri) else {
        return Ok(());
    };
    let lines = Lines::new(&doc.text);
    let n = doc.text.len();
    let diagnostics: Vec<_> = doc
        .analysis
        .diagnostics
        .iter()
        .map(|d| {
            // Errors inside the standard library are shown at the top of the file.
            let range = if d.span.end <= n {
                lines.range(d.span)
            } else {
                Range::default()
            };
            let mut message = d.message.clone();
            if let Some(h) = &d.help {
                message.push_str("\nhelp: ");
                message.push_str(h);
            }
            for note in &d.notes {
                message.push_str("\nnote: ");
                message.push_str(&note.0);
            }
            lsp_types::Diagnostic {
                range,
                severity: Some(match d.severity {
                    kiln_lexer::Severity::Error => DiagnosticSeverity::ERROR,
                    _ => DiagnosticSeverity::WARNING,
                }),
                source: Some("kiln".into()),
                message,
                ..Default::default()
            }
        })
        .collect();
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics,
        version: Some(doc.version),
    };
    conn.sender.send(Message::Notification(Notification::new(
        notification::PublishDiagnostics::METHOD.to_string(),
        params,
    )))?;
    Ok(())
}

fn update(docs: &mut HashMap<Uri, Doc>, uri: Uri, text: String, version: i32) {
    // The front end may panic on input it doesn't handle yet; keep serving.
    let analysis = std::panic::catch_unwind(|| analyze(&text)).unwrap_or_default();
    let ok = analysis.diagnostics.is_empty();
    let entry = docs.remove(&uri);
    let (good_members, good_type_names) = match (ok, entry) {
        (true, _) => (analysis.members.clone(), analysis.expr_type_names.clone()),
        (false, Some(d)) => (d.good_members, d.good_type_names),
        (false, None) => (analysis.members.clone(), analysis.expr_type_names.clone()),
    };
    docs.insert(
        uri,
        Doc {
            text,
            version,
            analysis,
            good_members,
            good_type_names,
        },
    );
}

fn handle_notification(docs: &mut HashMap<Uri, Doc>, n: Notification) -> Option<Uri> {
    match n.method.as_str() {
        notification::DidOpenTextDocument::METHOD => {
            let p: DidOpenTextDocumentParams = serde_json::from_value(n.params).ok()?;
            let uri = p.text_document.uri.clone();
            update(
                docs,
                uri.clone(),
                p.text_document.text,
                p.text_document.version,
            );
            Some(uri)
        }
        notification::DidChangeTextDocument::METHOD => {
            let p: DidChangeTextDocumentParams = serde_json::from_value(n.params).ok()?;
            let text = p.content_changes.into_iter().last()?.text;
            let uri = p.text_document.uri.clone();
            update(docs, uri.clone(), text, p.text_document.version);
            Some(uri)
        }
        notification::DidCloseTextDocument::METHOD => {
            let p: DidCloseTextDocumentParams = serde_json::from_value(n.params).ok()?;
            docs.remove(&p.text_document.uri);
            None
        }
        _ => None,
    }
}

fn ok<T: serde::Serialize>(id: RequestId, v: T) -> Response {
    Response::new_ok(id, v)
}

fn handle_request(docs: &HashMap<Uri, Doc>, req: Request) -> Response {
    let id = req.id.clone();
    let none = Response::new_ok(id.clone(), serde_json::Value::Null);
    let uri_of = |v: &serde_json::Value| -> Option<Uri> {
        serde_json::from_value(v.get("textDocument")?.get("uri")?.clone()).ok()
    };
    let Some(uri) = uri_of(&req.params) else {
        return none;
    };
    let Some(doc) = docs.get(&uri) else {
        return none;
    };
    let lines = Lines::new(&doc.text);
    let pos_of = |v: &serde_json::Value| -> Option<usize> {
        let p: Position = serde_json::from_value(v.get("position")?.clone()).ok()?;
        Some(lines.offset(p))
    };
    let a = &doc.analysis;
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
                        range: Some(lines.range(span)),
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
                Some(s) => ok(
                    id,
                    GotoDefinitionResponse::Scalar(Location::new(uri.clone(), lines.range(s))),
                ),
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
                .map(|s| Location::new(uri.clone(), lines.range(s)))
                .collect();
            ok(id, locs)
        }
        request::PrepareRenameRequest::METHOD => {
            let Some(at) = pos_of(&req.params) else {
                return none;
            };
            let refs = a.references(at);
            match refs.iter().find(|s| s.start <= at && at <= s.end) {
                Some(s) => ok(id, PrepareRenameResponse::Range(lines.range(*s))),
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
            let edits: Vec<TextEdit> = refs
                .into_iter()
                .map(|s| TextEdit::new(lines.range(s), new_name.clone()))
                .collect();
            let mut changes = HashMap::new();
            changes.insert(uri.clone(), edits);
            ok(
                id,
                WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                },
            )
        }
        request::DocumentSymbolRequest::METHOD => {
            #[allow(deprecated)]
            let syms: Vec<SymbolInformation> = a
                .symbols
                .iter()
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
                    location: Location::new(uri.clone(), lines.range(s.full)),
                    container_name: s.container.clone(),
                })
                .collect();
            ok(id, DocumentSymbolResponse::Flat(syms))
        }
        request::Formatting::METHOD => match kiln_fmt::format(&doc.text) {
            Ok(out) if out != doc.text => {
                let end = lines.position(doc.text.len());
                ok(
                    id,
                    vec![TextEdit::new(Range::new(Position::new(0, 0), end), out)],
                )
            }
            _ => ok(id, Vec::<TextEdit>::new()),
        },
        request::InlayHintRequest::METHOD => {
            let hints: Vec<InlayHint> = a
                .inlay_hints()
                .into_iter()
                .map(|(off, label)| InlayHint {
                    position: lines.position(off),
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
            let mut items = a.completions(&doc.text, at);
            if items.is_empty() {
                items = member_fallback(doc, at);
            }
            let items: Vec<CompletionItem> = items
                .into_iter()
                .map(|c| CompletionItem {
                    label: c.label,
                    kind: Some(match c.kind {
                        "method" => CompletionItemKind::METHOD,
                        "field" => CompletionItemKind::FIELD,
                        "local" => CompletionItemKind::VARIABLE,
                        "fn" => CompletionItemKind::FUNCTION,
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

/// `.` completion from the last analysis without errors: the expression
/// ending just before the dot, located by its text.
fn member_fallback(doc: &Doc, at: usize) -> Vec<Completion> {
    let before = &doc.text[..at.min(doc.text.len())];
    let word_start = before
        .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
        .len();
    let prefix = &before[word_start..];
    if !before[..word_start].ends_with('.') {
        return vec![];
    }
    let recv_end = word_start - 1;
    let recv = &before[..recv_end];
    let name_start = recv
        .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
        .len();
    let name = &recv[name_start..];
    if name.is_empty() {
        return vec![];
    }
    // A local or expression with the same name in the last good analysis.
    let ty = doc
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
        })
        .or_else(|| {
            doc.good_type_names
                .iter()
                .find(|(s, _)| doc.text.get(s.start..s.end) == Some(name))
                .map(|(_, t)| t.clone())
        });
    let Some(ty) = ty else { return vec![] };
    let mut v: Vec<Completion> = doc.good_members.get(&ty).cloned().unwrap_or_default();
    v.retain(|c| c.label.starts_with(prefix));
    v
}
