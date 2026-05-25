//! Walk a repo and extract symbol *definitions* + *references* per file using
//! tree-sitter.
//!
//! At the moment Rust is the only language with first-class queries. The
//! Python / JavaScript / TypeScript parsers are wired up and ready for
//! future extension; they currently contribute no definitions/references.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

use crate::{MapEntry, RepoMapError};

/// Kind of definition found in source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Type,
    Trait,
    Method,
    Const,
    Module,
}

/// One symbol definition discovered in the repo. Internal — converted to
/// [`MapEntry`] before being handed back to the caller.
#[derive(Debug, Clone)]
pub(crate) struct Definition {
    pub file: PathBuf,
    pub symbol: String,
    pub kind: SymbolKind,
    pub line: u32,
    pub signature: String,
}

impl Definition {
    /// Stable identifier used as a node label in the graph.
    pub(crate) fn node_id(&self) -> String {
        format!("{}::{}", self.file.display(), self.symbol)
    }
}

/// Output of [`walk_and_extract`].
#[derive(Debug, Default)]
pub(crate) struct Extraction {
    /// All discovered definitions (one per `(file, symbol)` pair — duplicates
    /// in the same file are deduplicated by `node_id`).
    pub definitions: Vec<Definition>,
    /// `file -> [referenced symbol names]`. Used to wire the graph: a file
    /// that mentions symbol `Foo` contributes an edge from every node in
    /// that file to every definition node whose `symbol == "Foo"`.
    pub references_by_file: HashMap<PathBuf, Vec<String>>,
    pub files_scanned: usize,
}

/// Walk `root` and extract definitions + references for every supported file.
pub(crate) fn walk_and_extract(
    root: &Path,
    extra_ignore: &[String],
) -> Result<Extraction, RepoMapError> {
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(true)
        .hidden(true)
        .git_ignore(true)
        .require_git(false);
    for glob in extra_ignore {
        // `ignore` exposes overrides via `OverrideBuilder`, but for the
        // small surface we need (extra ignores), pushing them as ignore
        // patterns to a custom `ignore::overrides::OverrideBuilder` is
        // overkill. Translate to gitignore-style add() instead.
        builder.add_custom_ignore_filename(glob);
    }
    if !extra_ignore.is_empty() {
        // Build a one-shot Override from the user-supplied globs.
        let mut overrides = ignore::overrides::OverrideBuilder::new(root);
        for glob in extra_ignore {
            // Prefix with `!` so the pattern *excludes*.
            let pat = if let Some(stripped) = glob.strip_prefix('!') {
                stripped.to_string()
            } else {
                format!("!{glob}")
            };
            if overrides.add(&pat).is_err() {
                // Skip invalid globs silently — they should not crash the
                // build.
                continue;
            }
        }
        if let Ok(ov) = overrides.build() {
            builder.overrides(ov);
        }
    }

    let mut extraction = Extraction::default();
    let mut parsers = Parsers::new()?;

    for entry in builder.build() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let Some(lang) = SupportedLang::detect(path) else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        // Cheap sanity guard — skip files bigger than ~1 MiB; tree-sitter
        // gets slow on giant minified files.
        if source.len() > 1_000_000 {
            continue;
        }
        extraction.files_scanned += 1;

        let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
        let (defs, refs) = parsers.parse_file(lang, &rel, &source)?;
        extraction
            .references_by_file
            .entry(rel.clone())
            .or_default()
            .extend(refs);
        extraction.definitions.extend(defs);
    }

    Ok(extraction)
}

/// Convert internal [`Definition`]s + a score map into the public [`MapEntry`].
pub(crate) fn into_entries(defs: Vec<Definition>, scores: &HashMap<String, f64>) -> Vec<MapEntry> {
    defs.into_iter()
        .map(|d| {
            let score = scores.get(&d.node_id()).copied().unwrap_or(0.0);
            MapEntry {
                file: d.file,
                symbol: d.symbol,
                kind: d.kind,
                line: d.line,
                signature: d.signature,
                score,
            }
        })
        .collect()
}

// -----------------------------------------------------------------------------
// Per-language plumbing
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum SupportedLang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
}

impl SupportedLang {
    fn detect(path: &Path) -> Option<Self> {
        match path.extension().and_then(|e| e.to_str())? {
            "rs" => Some(Self::Rust),
            "py" => Some(Self::Python),
            "js" | "mjs" | "cjs" | "jsx" => Some(Self::JavaScript),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            _ => None,
        }
    }
}

struct Parsers {
    rust_parser: Parser,
    rust_query: Query,
    rust_ref_query: Query,
    // Other languages: parsers are constructed but we don't yet have queries.
    py_parser: Parser,
    js_parser: Parser,
    ts_parser: Parser,
    tsx_parser: Parser,
}

impl Parsers {
    fn new() -> Result<Self, RepoMapError> {
        let rust_lang = tree_sitter_rust::LANGUAGE.into();
        let py_lang = tree_sitter_python::LANGUAGE.into();
        let js_lang = tree_sitter_javascript::LANGUAGE.into();
        let ts_lang = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let tsx_lang = tree_sitter_typescript::LANGUAGE_TSX.into();

        let mut rust_parser = Parser::new();
        rust_parser
            .set_language(&rust_lang)
            .map_err(|e| RepoMapError::Query(e.to_string()))?;
        let mut py_parser = Parser::new();
        py_parser
            .set_language(&py_lang)
            .map_err(|e| RepoMapError::Query(e.to_string()))?;
        let mut js_parser = Parser::new();
        js_parser
            .set_language(&js_lang)
            .map_err(|e| RepoMapError::Query(e.to_string()))?;
        let mut ts_parser = Parser::new();
        ts_parser
            .set_language(&ts_lang)
            .map_err(|e| RepoMapError::Query(e.to_string()))?;
        let mut tsx_parser = Parser::new();
        tsx_parser
            .set_language(&tsx_lang)
            .map_err(|e| RepoMapError::Query(e.to_string()))?;

        let rust_query = Query::new(&rust_lang, RUST_DEF_QUERY)
            .map_err(|e| RepoMapError::Query(e.to_string()))?;
        let rust_ref_query = Query::new(&rust_lang, RUST_REF_QUERY)
            .map_err(|e| RepoMapError::Query(e.to_string()))?;

        Ok(Self {
            rust_parser,
            rust_query,
            rust_ref_query,
            py_parser,
            js_parser,
            ts_parser,
            tsx_parser,
        })
    }

    fn parse_file(
        &mut self,
        lang: SupportedLang,
        rel_path: &Path,
        source: &str,
    ) -> Result<(Vec<Definition>, Vec<String>), RepoMapError> {
        match lang {
            SupportedLang::Rust => Ok(extract_rust(
                &mut self.rust_parser,
                &self.rust_query,
                &self.rust_ref_query,
                rel_path,
                source,
            )),
            SupportedLang::Python => {
                // Parse only — keeps the parser warm and acts as a smoke
                // test that the grammar still loads. Queries TBD.
                let _ = self.py_parser.parse(source, None);
                Ok((Vec::new(), Vec::new()))
            }
            SupportedLang::JavaScript => {
                let _ = self.js_parser.parse(source, None);
                Ok((Vec::new(), Vec::new()))
            }
            SupportedLang::TypeScript => {
                let _ = self.ts_parser.parse(source, None);
                Ok((Vec::new(), Vec::new()))
            }
            SupportedLang::Tsx => {
                let _ = self.tsx_parser.parse(source, None);
                Ok((Vec::new(), Vec::new()))
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Rust queries
// -----------------------------------------------------------------------------

/// Query for top-level Rust *definitions* we care about. Each pattern binds
/// `@name` (the identifier node) and `@def` (the enclosing node, used to
/// derive the signature). The pattern's capture index for `@kind.*` tells us
/// the symbol kind via the capture name.
const RUST_DEF_QUERY: &str = r#"
(function_item name: (identifier) @name) @def.function
(struct_item name: (type_identifier) @name) @def.type
(enum_item name: (type_identifier) @name) @def.type
(union_item name: (type_identifier) @name) @def.type
(type_item name: (type_identifier) @name) @def.type
(trait_item name: (type_identifier) @name) @def.trait
(mod_item name: (identifier) @name) @def.module
(const_item name: (identifier) @name) @def.const
(static_item name: (identifier) @name) @def.const
(function_signature_item name: (identifier) @name) @def.function
"#;

/// Query for Rust *references*: identifiers used at a call / use site.
///
/// We deliberately avoid the catch-all `(identifier) @ref` because it would
/// also match the identifier in `pub fn popular()` itself, giving every
/// defined symbol one inbound edge for free and flattening PageRank.
const RUST_REF_QUERY: &str = r#"
(call_expression function: (identifier) @ref)
(call_expression function: (scoped_identifier name: (identifier) @ref))
(call_expression function: (field_expression field: (field_identifier) @ref))
(macro_invocation macro: (identifier) @ref)
(scoped_identifier name: (identifier) @ref)
(scoped_type_identifier name: (type_identifier) @ref)
(generic_type type: (type_identifier) @ref)
(reference_type type: (type_identifier) @ref)
(use_list (identifier) @ref)
"#;

fn extract_rust(
    parser: &mut Parser,
    def_query: &Query,
    ref_query: &Query,
    rel_path: &Path,
    source: &str,
) -> (Vec<Definition>, Vec<String>) {
    let Some(tree) = parser.parse(source, None) else {
        return (Vec::new(), Vec::new());
    };
    let root = tree.root_node();
    let bytes = source.as_bytes();

    // --- Definitions ----------------------------------------------------
    let mut defs: Vec<Definition> = Vec::new();
    let mut cursor = QueryCursor::new();
    let capture_names: Vec<&str> = def_query.capture_names().to_vec();

    let mut matches = cursor.matches(def_query, root, bytes);
    while let Some(m) = matches.next() {
        // Locate the @name capture and the @def.* capture for this match.
        let mut name_node = None;
        let mut def_node = None;
        let mut kind = None;
        for cap in m.captures {
            let cname = capture_names[cap.index as usize];
            if cname == "name" {
                name_node = Some(cap.node);
            } else if let Some(suffix) = cname.strip_prefix("def.") {
                def_node = Some(cap.node);
                kind = Some(match suffix {
                    "function" => SymbolKind::Function,
                    "type" => SymbolKind::Type,
                    "trait" => SymbolKind::Trait,
                    "module" => SymbolKind::Module,
                    "const" => SymbolKind::Const,
                    _ => SymbolKind::Function,
                });
            }
        }
        let (Some(name_node), Some(def_node), Some(kind)) = (name_node, def_node, kind) else {
            continue;
        };
        let symbol = match name_node.utf8_text(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => continue,
        };
        let line = name_node.start_position().row as u32 + 1;
        let signature = signature_from_node(def_node, source);

        defs.push(Definition {
            file: rel_path.to_path_buf(),
            symbol,
            kind,
            line,
            signature,
        });
    }

    // Also pull methods out of `impl` blocks. The plain `function_item`
    // pattern above does not match `(impl_item ... (function_item ...))`
    // because tree-sitter-rust nests methods under `declaration_list`. To
    // keep the query simple we walk the tree once and pick up any
    // `function_item` whose parent chain includes `impl_item`, tagging it
    // as a Method.
    collect_methods(root, bytes, rel_path, source, &mut defs);

    // Dedup definitions by node id (file::symbol). Keep the first.
    let mut seen = std::collections::HashSet::new();
    defs.retain(|d| seen.insert(d.node_id()));

    // --- References -----------------------------------------------------
    let mut refs: Vec<String> = Vec::new();
    let mut ref_cursor = QueryCursor::new();
    let mut ref_matches = ref_cursor.matches(ref_query, root, bytes);
    while let Some(m) = ref_matches.next() {
        for cap in m.captures {
            if let Ok(text) = cap.node.utf8_text(bytes)
                && !text.is_empty()
            {
                refs.push(text.to_string());
            }
        }
    }
    // Dedup references *within* a file — repeated calls to the same symbol
    // shouldn't inflate edge weights.
    refs.sort();
    refs.dedup();

    (defs, refs)
}

fn collect_methods(
    root: tree_sitter::Node,
    bytes: &[u8],
    rel_path: &Path,
    source: &str,
    defs: &mut Vec<Definition>,
) {
    let mut stack = vec![(root, false)];
    while let Some((node, inside_impl)) = stack.pop() {
        let next_inside = inside_impl || node.kind() == "impl_item";
        if next_inside
            && node.kind() == "function_item"
            && let Some(name_node) = node.child_by_field_name("name")
            && let Ok(symbol) = name_node.utf8_text(bytes)
        {
            defs.push(Definition {
                file: rel_path.to_path_buf(),
                symbol: symbol.to_string(),
                kind: SymbolKind::Method,
                line: name_node.start_position().row as u32 + 1,
                signature: signature_from_node(node, source),
            });
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                stack.push((child, next_inside));
            }
        }
    }
}

/// Build a short, single-line "signature" by slicing source up to the body
/// (`{`, `;`, or end-of-line) and truncating to ~120 chars.
fn signature_from_node(node: tree_sitter::Node, source: &str) -> String {
    let start = node.start_byte();
    let bytes = source.as_bytes();
    let mut end = node.end_byte().min(bytes.len());
    // Walk forward from `start` to find the first `{`, `;`, or newline.
    let mut cut = end;
    for (i, &b) in bytes[start..end].iter().enumerate() {
        if b == b'{' || b == b';' || b == b'\n' {
            cut = start + i;
            break;
        }
    }
    end = cut;
    let mut sig = std::str::from_utf8(&bytes[start..end])
        .unwrap_or("")
        .trim()
        .to_string();
    // Collapse internal whitespace.
    sig = sig.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 120;
    if sig.len() > MAX {
        // Truncate on a char boundary.
        let mut idx = MAX;
        while !sig.is_char_boundary(idx) && idx > 0 {
            idx -= 1;
        }
        sig.truncate(idx);
        sig.push('…');
    }
    sig
}
