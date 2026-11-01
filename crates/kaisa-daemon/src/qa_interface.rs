//! Allow-listed black-box context extracted from a disposable frozen candidate checkout.
//! No diff, Git metadata, development test, transcript or developer answer is an input.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

/// Public documents and lexical API signatures. Source bodies are never included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicInterface {
    /// Relative path and public contents, in deterministic order.
    pub files: Vec<(String, String)>,
    /// Sandboxed help output, supplied by the QA runner.
    pub help: Vec<(String, String)>,
}

impl PublicInterface {
    /// Structured data for the isolated QA pack, with untrusted contents JSON-escaped.
    #[must_use]
    pub fn as_json(&self) -> Value {
        json!({"files": self.files.iter().map(|(path, text)| json!({"path":path,"text":text})).collect::<Vec<_>>(),
            "cli_help": self.help.iter().map(|(command, text)| json!({"command":command,"text":text})).collect::<Vec<_>>()})
    }
}

/// Extracts README/docs, Rust public signatures and Node package entry-point exports.
/// Call only on a fresh checkout of the frozen candidate. Help execution is a separate,
/// sandboxed runner operation: this function never executes candidate-controlled code.
///
/// # Errors
/// Rejects symlink escapes, malformed package metadata and unreadable public files.
pub fn extract_public_interface(checkout: &Path) -> io::Result<PublicInterface> {
    let mut files = Vec::new();
    let mut paths = Vec::new();
    documents(checkout, Path::new(""), &mut paths)?;
    paths.sort();
    for path in paths {
        let text = read_public(checkout, &path)?;
        files.push((path.to_string_lossy().into_owned(), text));
    }
    let manifest = rust_manifest(checkout)?;
    let lib = manifest
        .as_ref()
        .and_then(|manifest| manifest.get("lib"))
        .and_then(|lib| lib.get("path"))
        .and_then(toml::Value::as_str)
        .unwrap_or("src/lib.rs");
    if checkout.join(lib).is_file() {
        rust_module(
            checkout,
            Path::new(lib),
            &mut BTreeSet::new(),
            &mut files,
            true,
        )?;
    }
    if let Some(manifest) = &manifest {
        let package = manifest.get("package");
        files.push(("Cargo.toml".into(), json!({"package_name":package.and_then(|p| p.get("name")).and_then(toml::Value::as_str),
            "library_name":manifest.get("lib").and_then(|p| p.get("name")).and_then(toml::Value::as_str),
            "library_path":lib}).to_string()));
    }
    let package_path = Path::new("package.json");
    if checkout.join(package_path).is_file() {
        let package: Value = serde_json::from_str(&read_public(checkout, package_path)?)?;
        let mut public = serde_json::Map::new();
        let mut modules = BTreeSet::new();
        for key in ["name", "type", "bin", "main", "exports"] {
            if let Some(value) = package.get(key) {
                public.insert(key.to_owned(), value.clone());
                if matches!(key, "bin" | "main" | "exports") {
                    module_paths(value, &mut modules);
                }
            }
        }
        if package.get("main").is_none()
            && package.get("exports").is_none()
            && checkout.join("index.js").is_file()
        {
            modules.insert("index.js".to_owned());
        }
        files.push(("package.json".to_owned(), Value::Object(public).to_string()));
        for module in modules {
            let path = Path::new(&module);
            if excluded(path) {
                continue;
            }
            let text = read_public(checkout, path)?;
            files.push((module, node_exports(&text)));
        }
    }
    files.sort();
    files.dedup();
    Ok(PublicInterface {
        files,
        help: Vec::new(),
    })
}

fn rust_manifest(checkout: &Path) -> io::Result<Option<toml::Value>> {
    if !checkout.join("Cargo.toml").is_file() {
        return Ok(None);
    }
    let manifest = read_public(checkout, Path::new("Cargo.toml"))?;
    toml::from_str(&manifest)
        .map(Some)
        .map_err(io::Error::other)
}

/// Explicit bins and Cargo's ordinary main/bin conventions, without running Cargo metadata
/// or exposing manifest scripts, dependencies, development settings or source bodies.
pub(crate) fn rust_bins(checkout: &Path) -> io::Result<Vec<(String, String)>> {
    let Some(manifest) = rust_manifest(checkout)? else {
        return Ok(Vec::new());
    };
    let mut bins = Vec::new();
    if let Some(explicit) = manifest.get("bin").and_then(toml::Value::as_array) {
        for bin in explicit {
            let name = bin
                .get("name")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| io::Error::other("Rust bin has no name"))?;
            let path = bin
                .get("path")
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("src/bin/{name}.rs"));
            read_public(checkout, Path::new(&path))?;
            bins.push((name.to_owned(), path));
        }
    }
    let package = manifest.get("package");
    let autobins = package
        .and_then(|p| p.get("autobins"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(true);
    if autobins {
        if checkout.join("src/main.rs").is_file() {
            read_public(checkout, Path::new("src/main.rs"))?;
            let name = package
                .and_then(|p| p.get("name"))
                .and_then(toml::Value::as_str)
                .ok_or_else(|| io::Error::other("Rust package has no name"))?;
            if !bins.iter().any(|(bin, _)| bin == name) {
                bins.push((name.to_owned(), "src/main.rs".into()));
            }
        }
        let directory = checkout.join("src/bin");
        if directory.is_dir() {
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                let path = if entry.file_type()?.is_dir() {
                    entry.path().join("main.rs")
                } else {
                    entry.path()
                };
                if !path.is_file() || path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let name = if entry.file_type()?.is_dir() {
                    entry.file_name().to_string_lossy().into_owned()
                } else {
                    path.file_stem()
                        .expect("Rust bin stem")
                        .to_string_lossy()
                        .into_owned()
                };
                let path = path.strip_prefix(checkout).map_err(io::Error::other)?;
                read_public(checkout, path)?;
                if !bins.iter().any(|(bin, _)| bin == &name) {
                    bins.push((name, path.to_string_lossy().into_owned()));
                }
            }
        }
    }
    bins.sort();
    Ok(bins)
}

fn module_paths(value: &Value, paths: &mut BTreeSet<String>) {
    match value {
        Value::String(path) => {
            paths.insert(path.trim_start_matches("./").to_owned());
        }
        Value::Array(values) => {
            for value in values {
                module_paths(value, paths);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                module_paths(value, paths);
            }
        }
        _ => {}
    }
}

/// The complete Node bin map, including a package's single string bin.
pub(crate) fn node_bins(checkout: &Path) -> io::Result<Vec<String>> {
    if !checkout.join("package.json").is_file() {
        return Ok(Vec::new());
    }
    let package: Value = serde_json::from_str(&read_public(checkout, Path::new("package.json"))?)?;
    let mut paths = BTreeSet::new();
    if let Some(bin) = package.get("bin") {
        module_paths(bin, &mut paths);
    }
    for path in &paths {
        read_public(checkout, Path::new(path))?;
    }
    Ok(paths.into_iter().collect())
}

fn documents(root: &Path, relative: &Path, output: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        if excluded(&path) || entry.file_type()?.is_symlink() {
            continue;
        }
        let lower = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if entry.file_type()?.is_dir() {
            if path.starts_with("docs") {
                documents(root, &path, output)?;
            }
        } else if lower == "readme"
            || lower.starts_with("readme.")
            || (path.starts_with("docs")
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        [
                            "md", "markdown", "txt", "rst", "adoc", "html", "htm", "json", "yaml",
                            "yml",
                        ]
                        .contains(&extension)
                    }))
        {
            output.push(path);
        }
    }
    Ok(())
}

fn excluded(path: &Path) -> bool {
    path.components().any(|part| {
        let name = part.as_os_str().to_string_lossy().to_ascii_lowercase();
        name.starts_with('.') && name != "."
            || [
                "test",
                "tests",
                "__tests__",
                "target",
                "node_modules",
                "history",
                "revisions",
            ]
            .contains(&name.as_str())
            || [
                "transcript",
                "final_answer",
                "final-answer",
                "commit-message",
                "commit_message",
                "developer-reasoning",
                "developer_reasoning",
                "developer-tests",
                "developer_tests",
                "test-results",
                "test_results",
            ]
            .iter()
            .any(|marker| name.contains(marker))
            || name.contains(".test.")
            || name.contains(".spec.")
            || name.ends_with("_test.rs")
    })
}

pub(crate) fn read_public(root: &Path, path: &Path) -> io::Result<String> {
    if path.is_absolute()
        || excluded(path)
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(io::Error::other("public interface path is not allowed"));
    }
    let root = root.canonicalize()?;
    let mut file = root.clone();
    for part in path.components() {
        file.push(part);
        if std::fs::symlink_metadata(&file)?.file_type().is_symlink() {
            return Err(io::Error::other(
                "public interface paths must not be symlinks",
            ));
        }
    }
    if !file.canonicalize()?.starts_with(root) {
        return Err(io::Error::other("public interface escaped checkout"));
    }
    std::fs::read_to_string(file)
}

#[derive(Clone)]
struct Token {
    text: String,
    doc: bool,
}

/// Code tokens with comments and literal contents omitted, for conservative QA scans.
pub(crate) fn code_tokens(source: &str, javascript: bool) -> Vec<String> {
    tokens(source, javascript)
        .into_iter()
        .filter(|token| !token.doc)
        .map(|token| token.text)
        .collect()
}

/// Lexical scan: comments and string contents cannot manufacture `pub` or `export` tokens.
fn tokens(text: &str, keep_jsdoc: bool) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if bytes[at..].starts_with(b"//") {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        if bytes[at..].starts_with(b"/*") {
            let doc = keep_jsdoc && bytes[at..].starts_with(b"/**");
            at += 2;
            let mut depth = 1;
            while at < bytes.len() && depth > 0 {
                if bytes[at..].starts_with(b"/*") {
                    depth += 1;
                    at += 2;
                } else if bytes[at..].starts_with(b"*/") {
                    depth -= 1;
                    at += 2;
                } else {
                    at += 1;
                }
            }
            if doc {
                out.push(Token {
                    text: text[start..at].to_owned(),
                    doc: true,
                });
            }
            continue;
        }
        if keep_jsdoc
            && bytes[at] == b'/'
            && out.last().is_none_or(|token: &Token| {
                ["=", "(", "[", ":", ",", "return", "!", ">"].contains(&token.text.as_str())
            })
        {
            // A JavaScript regex literal is opaque too; reserved words in its pattern are
            // data, not declarations. Character classes may contain unescaped slashes.
            at += 1;
            let mut character_class = false;
            while at < bytes.len() {
                if bytes[at] == b'\\' {
                    at = (at + 2).min(bytes.len());
                    continue;
                }
                if bytes[at] == b'[' {
                    character_class = true;
                }
                if bytes[at] == b']' {
                    character_class = false;
                }
                if bytes[at] == b'/' && !character_class {
                    at += 1;
                    break;
                }
                at += 1;
            }
            while bytes.get(at).is_some_and(u8::is_ascii_alphabetic) {
                at += 1;
            }
            out.push(Token {
                text: "<literal>".into(),
                doc: false,
            });
            continue;
        }
        // Rust raw strings, including byte raw strings, are single opaque tokens.
        let raw = if bytes[at..].starts_with(b"br") {
            Some(at + 2)
        } else if bytes[at] == b'r' {
            Some(at + 1)
        } else {
            None
        };
        if let Some(mut quote) = raw {
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if bytes.get(quote) == Some(&b'"') {
                let hashes = quote - raw.expect("raw prefix");
                at = quote + 1;
                while at < bytes.len() {
                    if bytes[at] == b'"'
                        && bytes
                            .get(at + 1..at + 1 + hashes)
                            .is_some_and(|tail| tail.iter().all(|b| *b == b'#'))
                    {
                        at += 1 + hashes;
                        break;
                    }
                    at += 1;
                }
                out.push(Token {
                    text: "<literal>".into(),
                    doc: false,
                });
                continue;
            }
        }
        // Apostrophes on Rust lifetimes are not string delimiters.
        let quote = bytes[at];
        let char_literal = quote == b'\''
            && (bytes.get(at + 2) == Some(&b'\'') || bytes.get(at + 1) == Some(&b'\\'));
        if quote == b'"' || quote == b'`' || (keep_jsdoc && quote == b'\'') || char_literal {
            at += 1;
            while at < bytes.len() {
                if bytes[at] == b'\\' {
                    at = (at + 2).min(bytes.len());
                } else if bytes[at] == quote {
                    at += 1;
                    break;
                } else {
                    at += 1;
                }
            }
            out.push(Token {
                text: "<literal>".into(),
                doc: false,
            });
            continue;
        }
        if bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' {
            at += 1;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
        } else {
            at += 1;
            while !text.is_char_boundary(at) {
                at += 1;
            }
        }
        out.push(Token {
            text: text[start..at].to_owned(),
            doc: false,
        });
    }
    out
}

fn closing(tokens: &[Token], start: usize) -> usize {
    let open = tokens[start].text.as_str();
    let close = match open {
        "{" => "}",
        "[" => "]",
        "(" => ")",
        _ => return start,
    };
    let mut depth = 0;
    for (i, token) in tokens.iter().enumerate().skip(start) {
        if token.text == open {
            depth += 1;
        }
        if token.text == close {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
    }
    tokens.len().saturating_sub(1)
}
fn joined(tokens: &[Token]) -> String {
    tokens
        .iter()
        .filter(|t| !t.doc)
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .replace(": :", "::")
        .replace(" :: ", "::")
        .replace("- >", "->")
        .replace("= >", "=>")
        .replace("' ", "'")
}

fn rust_module(
    root: &Path,
    path: &Path,
    seen: &mut BTreeSet<PathBuf>,
    files: &mut Vec<(String, String)>,
    crate_root: bool,
) -> io::Result<()> {
    if !seen.insert(path.to_owned()) {
        return Ok(());
    }
    let text = read_public(root, path)?;
    let tokens = tokens(&text, false);
    let directory = if crate_root
        || path
            .file_name()
            .is_some_and(|name| name == "lib.rs" || name == "mod.rs")
    {
        path.parent().unwrap_or(Path::new("")).to_owned()
    } else {
        path.with_extension("")
    };
    let mut signatures = Vec::new();
    rust_scope(
        root,
        &directory,
        &tokens,
        seen,
        files,
        &mut signatures,
        false,
    )?;
    files.push((path.to_string_lossy().into_owned(), signatures.join("\n")));
    Ok(())
}

fn rust_scope(
    root: &Path,
    directory: &Path,
    ts: &[Token],
    seen: &mut BTreeSet<PathBuf>,
    files: &mut Vec<(String, String)>,
    signatures: &mut Vec<String>,
    implicit_public: bool,
) -> io::Result<()> {
    let mut at = 0;
    while at < ts.len() {
        let mut test_only = false;
        while at + 1 < ts.len() && ts[at].text == "#" && ts[at + 1].text == "[" {
            let end = closing(ts, at + 1);
            test_only |= ts[at..=end]
                .iter()
                .any(|t| t.text == "test" || t.text == "ignore");
            at = end + 1;
        }
        if at >= ts.len() {
            break;
        }
        let start = at;
        let public = implicit_public
            || (ts[at].text == "pub" && ts.get(at + 1).is_none_or(|t| t.text != "("));
        let implementation = ts[at].text == "impl";
        let mut end = at;
        while end < ts.len() && !["{", ";", "="].contains(&ts[end].text.as_str()) {
            if ts[end].text == "(" || ts[end].text == "[" {
                end = closing(ts, end);
            }
            end += 1;
        }
        if end >= ts.len() {
            break;
        }
        let module = public && ts.get(start + 1).is_some_and(|t| t.text == "mod");
        if public && !test_only {
            if ts[end].text == "=" && ts[start..end].iter().any(|t| t.text == "type") {
                let mut alias_end = end;
                while alias_end < ts.len() && ts[alias_end].text != ";" {
                    alias_end += 1;
                }
                signatures.push(joined(&ts[start..alias_end]));
            } else {
                signatures.push(joined(&ts[start..end]));
            }
        }
        if ts[end].text == "{" {
            let close = closing(ts, end);
            // Function bodies and private modules are never scanned for public-looking text.
            let container = ts[start..end]
                .iter()
                .any(|t| ["mod", "struct", "enum", "trait"].contains(&t.text.as_str()));
            if !test_only && (implementation || (public && container)) {
                let next = if module {
                    directory.join(&ts[start + 2].text)
                } else {
                    directory.to_owned()
                };
                let body = &ts[end + 1..close];
                let kind = ts[start..end]
                    .iter()
                    .find(|t| ["struct", "enum", "trait"].contains(&t.text.as_str()))
                    .map(|t| t.text.as_str());
                match kind {
                    Some("struct") => signatures.extend(rust_fields(body, false)),
                    Some("enum") => signatures.extend(rust_fields(body, true)),
                    _ => {
                        if implementation {
                            signatures.push(joined(&ts[start..end]));
                        }
                        rust_scope(
                            root,
                            &next,
                            body,
                            seen,
                            files,
                            signatures,
                            kind == Some("trait"),
                        )?;
                    }
                }
            }
            at = close + 1;
        } else {
            if module && !test_only && ts[end].text == ";" {
                let name = &ts[start + 2].text;
                let flat = directory.join(format!("{name}.rs"));
                let nested = directory.join(name).join("mod.rs");
                rust_module(
                    root,
                    if root.join(&flat).is_file() {
                        &flat
                    } else {
                        &nested
                    },
                    seen,
                    files,
                    false,
                )?;
            }
            // Skip const initializers and expression bodies without exposing their contents.
            while end < ts.len() && ts[end].text != ";" {
                if ["{", "(", "["].contains(&ts[end].text.as_str()) {
                    end = closing(ts, end);
                }
                end += 1;
            }
            at = end + 1;
        }
    }
    Ok(())
}

/// Fields and enum variants contain type declarations, not function bodies. Initializers
/// (enum discriminants) are omitted and all string literals remain opaque lexer tokens.
fn rust_fields(ts: &[Token], implicit_public: bool) -> Vec<String> {
    let mut output = Vec::new();
    let mut start = 0;
    let mut at = 0;
    let mut angles: usize = 0;
    while at <= ts.len() {
        if at == ts.len() || (ts[at].text == "," && angles == 0) {
            let field = &ts[start..at];
            let mut first = 0;
            while first + 1 < field.len()
                && field[first].text == "#"
                && field[first + 1].text == "["
            {
                first = closing(field, first + 1) + 1;
            }
            if first < field.len()
                && (implicit_public
                    || (field[first].text == "pub"
                        && field.get(first + 1).is_none_or(|t| t.text != "(")))
            {
                let end = field
                    .iter()
                    .position(|t| t.text == "=")
                    .unwrap_or(field.len());
                output.push(joined(&field[first..end]));
            }
            start = at + 1;
        } else {
            match ts[at].text.as_str() {
                "<" => angles += 1,
                ">" => angles = angles.saturating_sub(1),
                "(" | "[" | "{" => at = closing(ts, at),
                _ => {}
            }
        }
        at += 1;
    }
    output
}

/// Associates top-level declaration docs with their names. Export lists and CommonJS
/// shorthand can expose a function declared earlier; only docs for referenced names survive.
fn node_declaration_docs(ts: &[Token]) -> BTreeMap<String, String> {
    let mut docs = BTreeMap::new();
    let mut pending = None;
    let mut at = 0;
    while at < ts.len() {
        if ts[at].doc {
            pending = Some(ts[at].text.clone());
        } else if ["export", "default", "async"].contains(&ts[at].text.as_str()) { /* declaration prefixes */
        } else if ["function", "const", "let", "var"].contains(&ts[at].text.as_str()) {
            if let Some(doc) = pending.take()
                && let Some(name) = ts.get(at + 1)
            {
                docs.insert(name.text.clone(), doc);
            }
        } else {
            pending = None;
        }
        if ["{", "(", "["].contains(&ts[at].text.as_str()) {
            at = closing(ts, at);
        }
        at += 1;
    }
    docs
}

fn node_exports(source: &str) -> String {
    let ts = tokens(source, true);
    let docs = node_declaration_docs(&ts);
    let mut references = BTreeSet::new();
    let mut output = Vec::new();
    let mut at = 0;
    let mut doc = None;
    while at < ts.len() {
        if ts[at].doc {
            doc = Some(ts[at].text.clone());
            at += 1;
            continue;
        }
        let start = at;
        let export = ts[at].text == "export";
        let common = ts[at].text == "exports"
            || (ts[at].text == "module" && ts.get(at + 2).is_some_and(|t| t.text == "exports"));
        let mut end = at;
        while end < ts.len() && !["{", ";", "="].contains(&ts[end].text.as_str()) {
            end += 1;
        }
        if export || common {
            if let Some(doc) = doc.take() {
                output.push(doc);
            }
            output.push(joined(&ts[start..end]));
            if export
                && let Some(kind) = ts[start..end].iter().position(|token| {
                    ["function", "const", "let", "var"].contains(&token.text.as_str())
                })
                && let Some(name) = ts.get(start + kind + 1)
            {
                references.insert(name.text.clone());
            }
            if common && ts.get(end).is_some_and(|t| t.text == "=") {
                if ts.get(end + 1).is_some_and(|t| t.text == "{") {
                    let close = closing(&ts, end + 1);
                    // Object keys identify the exported functions. Never include values/bodies.
                    let mut key = true;
                    let mut index = end + 2;
                    while index < close {
                        if ts[index].doc {
                            output.push(ts[index].text.clone());
                        }
                        if key
                            && ts[index]
                                .text
                                .chars()
                                .all(|c| c.is_alphanumeric() || c == '_')
                        {
                            output.push(format!("export {}", ts[index].text));
                            let local = if ts.get(index + 1).is_some_and(|token| token.text == ":")
                            {
                                ts.get(index + 2)
                            } else {
                                ts.get(index)
                            };
                            if let Some(local) = local {
                                references.insert(local.text.clone());
                            }
                            key = false;
                        }
                        if ["{", "(", "["].contains(&ts[index].text.as_str()) {
                            index = closing(&ts, index);
                        }
                        if ts[index].text == "," {
                            key = true;
                        }
                        index += 1;
                    }
                } else if let Some(local) = ts
                    .get(end + 1)
                    .filter(|token| docs.contains_key(&token.text))
                {
                    references.insert(local.text.clone());
                    output.push(format!("export {}", local.text));
                } else if ts.get(end + 1).is_some_and(|t| t.text == "function")
                    && let Some(name) = ts.get(end + 2).filter(|t| t.text != "(")
                {
                    output.push(format!("export {}", name.text));
                }
            }
            if export
                && ts.get(end).is_some_and(|t| t.text == "{")
                && ts.get(start + 1).is_some_and(|t| t.text == "{")
            {
                let close = closing(&ts, end);
                let mut first = true;
                for token in &ts[end + 1..close] {
                    if first {
                        references.insert(token.text.clone());
                        first = false;
                    }
                    if token.text == "," {
                        first = true;
                    }
                }
                output.push(joined(&ts[end..=close]));
            }
        }
        doc = None;
        // Walk top-level tokens rather than relying on semicolons: JavaScript's automatic
        // semicolon insertion must not make an import hide the following export. Skip all
        // nested scopes so implementation bodies cannot manufacture exported names.
        if export || common {
            at = end.max(at + 1);
        } else if ["{", "(", "["].contains(&ts[at].text.as_str()) {
            at = closing(&ts, at) + 1;
        } else {
            at += 1;
        }
    }
    for reference in references {
        if let Some(doc) = docs.get(&reference)
            && !output.contains(doc)
        {
            output.push(doc.clone());
        }
    }
    output.join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{extract_public_interface, node_exports};

    struct PublicFixture(PathBuf);
    impl PublicFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "kaisa-qa-interface-{}-{}",
                std::process::id(),
                crate::ids::fresh_suffix()
            ));
            std::fs::create_dir_all(&root).expect("public fixture");
            Self(root)
        }
        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("fixture parent");
            std::fs::write(path, text).expect("fixture file");
        }
    }
    impl Drop for PublicFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn a_public_interface_excludes_developer_tests_reasoning_transcripts_answers_and_history() {
        let fixture = PublicFixture::new();
        fixture.write("README.md", "Public usage");
        fixture.write("docs/api.md", "Public API");
        for path in [
            "tests/secret.rs",
            "docs/tests/dev.md",
            "docs/transcript.md",
            "docs/final-answer.md",
            "docs/developer-tests.md",
            "docs/test-results.md",
            "docs/commit-message.md",
            ".git/logs/HEAD",
        ] {
            fixture.write(path, "FORBIDDEN_DEVELOPER_REASONING");
        }
        fixture.write("src/lib.rs", "pub mod api; mod private { pub fn private_body() {} } #[cfg(test)] pub mod tests { pub fn FORBIDDEN_TEST_RESULT() {} } pub fn visible(input: &str) -> bool { let reasoning = \"FORBIDDEN_DEVELOPER_REASONING\"; true } // pub fn FORGED_SIGNATURE() {}\nconst TEXT: &str = r###\"pub fn FORGED_LITERAL() {}\"###;");
        fixture.write("src/api.rs", "pub fn reject_empty(input: &str) -> Result<(), Error> { panic!(\"FORBIDDEN_IMPLEMENTATION\"); }");
        fixture.write(
            "package.json",
            r#"{"main":"index.js","scripts":{"test":"FORBIDDEN_TEST_RESULT"}}"#,
        );
        fixture.write("index.js", "/** Public parse contract. */ export function parse(input) { const reasoning = 'FORBIDDEN_DEVELOPER_REASONING'; }\nconst privateValue = 'FORBIDDEN_IMPLEMENTATION';");
        let interface = extract_public_interface(&fixture.0).expect("public interface");
        let pack = interface.as_json().to_string();
        assert!(
            !pack.contains("FORBIDDEN")
                && !pack.contains("FORGED")
                && !pack.contains("private_body"),
            "no development body, test, transcript, answer, commit or history enters QA: {pack}"
        );
        for public in [
            "Public usage",
            "Public API",
            "reject_empty",
            "visible",
            "parse",
            "Public parse contract",
        ] {
            assert!(
                pack.contains(public),
                "public contract {public} survives extraction"
            );
        }
    }
    #[test]
    fn javascript_strings_and_comments_cannot_forge_exported_functions() {
        let source = "const text = 'export function forged() {}'; const pattern = /export function forged_regex()/; /* export function forged_comment() {} */\n/** Real contract. */ export const parse = (input) => input;";
        let public = node_exports(source);
        assert!(
            public.contains("parse") && public.contains("Real contract"),
            "the exported function and JSDoc are public"
        );
        assert!(
            !public.contains("forged"),
            "literals and ordinary comments must not create fake API declarations"
        );
    }
    #[test]
    fn public_rust_types_traits_and_impl_signatures_preserve_the_callable_contract() {
        let fixture = PublicFixture::new();
        fixture.write(
            "Cargo.toml",
            "[package]\nname = \"public-contract\"\nversion = \"0.1.0\"\n",
        );
        fixture.write("src/lib.rs", "pub struct Request { pub value: Option<Result<u8, Error>>, private_value: String } pub enum Error { Empty, Malformed { field: String } } pub trait Parse { fn parse(input: &str) -> Result<Request, Error>; fn default_parse() { let private_reasoning = \"FORBIDDEN_BODY\"; } } impl Request { pub fn new(value: u8) -> Self { todo!() } } pub type ResultAlias = Result<Request, Error>; pub(crate) fn FORBIDDEN_INTERNAL() {}");
        let public = extract_public_interface(&fixture.0)
            .expect("lexical contract")
            .as_json()
            .to_string();
        for item in [
            "public-contract",
            "value",
            "Empty",
            "Malformed",
            "parse",
            "default_parse",
            "new",
            "ResultAlias",
            "Result < Request",
        ] {
            assert!(
                public.contains(item),
                "public type or method {item} is needed by a black-box client: {public}"
            );
        }
        assert!(
            !public.contains("FORBIDDEN") && !public.contains("private_value"),
            "private bodies, restricted pub items and private fields must stay excluded: {public}"
        );
    }

    #[test]
    fn node_exports_survive_automatic_semicolons_and_include_commonjs_names_without_bodies() {
        let source = "import input from 'input'\n/** Public parser. */\nexport function parse(value) { return 'FORBIDDEN_BODY'; }\nexport const validate = value => true\nmodule.exports = { recover, concurrent: function concurrent() { return 'FORBIDDEN_BODY'; } }\nexports.reject = function reject(value) { return 'FORBIDDEN_BODY'; }";
        let public = node_exports(source);
        for name in [
            "parse",
            "validate",
            "recover",
            "concurrent",
            "reject",
            "Public parser",
        ] {
            assert!(
                public.contains(name),
                "export {name} survives lexical extraction: {public}"
            );
        }
        assert!(
            !public.contains("FORBIDDEN_BODY"),
            "export names never require implementation source"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_public_entry_symlink_cannot_expose_tests_or_files_outside_the_candidate() {
        let fixture = PublicFixture::new();
        fixture.write("package.json", r#"{"main":"index.js"}"#);
        fixture.write("tests/private.js", "FORBIDDEN_TEST_SOURCE");
        std::os::unix::fs::symlink("tests/private.js", fixture.0.join("index.js"))
            .expect("hostile public entry symlink");
        assert!(
            extract_public_interface(&fixture.0).is_err(),
            "public entry aliases must never reopen excluded developer tests"
        );
    }

    #[test]
    fn later_export_lists_and_commonjs_shorthand_retain_only_the_exported_functions_jsdoc() {
        let source = "/** Public parser contract. */ function parse(value) { return value; } /** FORBIDDEN_PRIVATE_REASONING */ function privateHelper(value) { return value; } module.exports = { parse }; /** Public recovery contract. */ const recover = value => value; export { recover as restore };";
        let public = node_exports(source);
        for contract in [
            "Public parser contract",
            "Public recovery contract",
            "parse",
            "restore",
        ] {
            assert!(
                public.contains(contract),
                "exported contract {contract} survives separate declaration and export: {public}"
            );
        }
        assert!(
            !public.contains("FORBIDDEN_PRIVATE_REASONING") && !public.contains("privateHelper"),
            "private declaration docs must not enter QA through unrelated exports"
        );
    }

    #[test]
    fn binary_documentation_assets_do_not_prevent_extracting_textual_public_docs() {
        let fixture = PublicFixture::new();
        fixture.write("docs/api.md", "Public text contract");
        std::fs::write(fixture.0.join("docs/diagram.png"), [0xff, 0x00])
            .expect("binary public doc asset");
        let public = extract_public_interface(&fixture.0)
            .expect("textual public contract extraction")
            .as_json()
            .to_string();
        assert!(
            public.contains("Public text contract") && !public.contains("diagram.png"),
            "the text-only QA pack must remain usable when docs include images"
        );
    }

    #[test]
    fn custom_rust_library_roots_resolve_public_modules_and_keep_signature_operators() {
        let fixture = PublicFixture::new();
        fixture.write(
            "Cargo.toml",
            "[package]\nname = \"custom-api\"\nversion = \"0.1.0\"\n[lib]\npath = \"api.rs\"\n",
        );
        fixture.write("api.rs", "pub mod contract; pub fn borrowed<'a>(input: &'a std::path::Path) -> &'a std::path::Path { input }");
        fixture.write(
            "contract.rs",
            "pub fn reject_empty(input: &str) -> bool { !input.is_empty() }",
        );
        let public = extract_public_interface(&fixture.0)
            .expect("custom crate-root interface")
            .as_json()
            .to_string();
        for signature in ["reject_empty", "std::path::Path", "->", "'a"] {
            assert!(
                public.contains(signature),
                "the lexical signature preserves usable public syntax {signature}: {public}"
            );
        }
    }
}
