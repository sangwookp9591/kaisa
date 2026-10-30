//! `kaisa agent add|list`: personal agent registrations, with no admin (T27a, user-flows F37).
//! Entries are validated by the domain catalog before they are saved, so an unsafe endpoint
//! or a built-in name never reaches the file.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kaisa_domain::{AgentEntry, AgentKind, Catalog, CatalogModel, Grade};
use serde_json::{Value, json};

/// The file in the state directory holding personal registrations.
const CATALOG_FILE: &str = "catalog.json";

fn grade_from(code: &str) -> Option<Grade> {
    match code {
        "T0" => Some(Grade::T0),
        "T1" => Some(Grade::T1),
        "T2" => Some(Grade::T2),
        "T3" => Some(Grade::T3),
        _ => None,
    }
}

fn entry_to_json(entry: &AgentEntry) -> Value {
    let (kind, target) = match &entry.kind {
        AgentKind::CliAgent { program } => ("cli-agent", program.as_str()),
        AgentKind::OpenAiCompatible { endpoint } => ("openai-compatible", endpoint.as_str()),
    };
    json!({
        "name": entry.name,
        "kind": kind,
        "target": target,
        "models": entry.models.iter()
            .map(|model| json!({"id": model.id, "grade": format!("{:?}", model.grade)}))
            .collect::<Vec<_>>(),
    })
}

fn entry_from_json(value: &Value) -> Option<AgentEntry> {
    let target: String = value["target"].as_str()?.to_owned();
    let kind = match value["kind"].as_str()? {
        "cli-agent" => AgentKind::CliAgent { program: target },
        "openai-compatible" => AgentKind::OpenAiCompatible { endpoint: target },
        _ => return None,
    };
    let models = value["models"]
        .as_array()?
        .iter()
        .map(|model| {
            Some(CatalogModel {
                id: model["id"].as_str()?.to_owned(),
                grade: grade_from(model["grade"].as_str()?)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(AgentEntry {
        name: value["name"].as_str()?.to_owned(),
        kind,
        models,
        personal: true,
    })
}

/// The personal catalog saved in `state`; a missing file is an empty catalog.
fn load(state: &Path) -> io::Result<Catalog> {
    let mut catalog = Catalog::new();
    let text = match std::fs::read_to_string(state.join(CATALOG_FILE)) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(catalog),
        Err(error) => return Err(error),
    };
    let entries: Vec<Value> = serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    for value in &entries {
        let entry = entry_from_json(value).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "an entry in catalog.json is malformed",
            )
        })?;
        catalog
            .add(entry)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    }
    Ok(catalog)
}

fn save(state: &Path, catalog: &Catalog) -> io::Result<()> {
    kaisa_sys::ensure_private_directory(state)?;
    let entries: Vec<Value> = catalog.entries().iter().map(entry_to_json).collect();
    std::fs::write(state.join(CATALOG_FILE), Value::Array(entries).to_string())
}

/// Parsed `kaisa agent add <name> (--program <p> | --endpoint <url>) --model <id>=<T0..T3>…`.
fn parse_add(args: &[String]) -> Result<AgentEntry, String> {
    let name: String = args.first().ok_or("missing agent name")?.clone();
    let mut kind: Option<AgentKind> = None;
    let mut models: Vec<CatalogModel> = Vec::new();
    let mut words = args[1..].iter();
    while let Some(word) = words.next() {
        let value: &String = words.next().ok_or(format!("{word} needs a value"))?;
        match word.as_str() {
            "--program" => {
                kind = Some(AgentKind::CliAgent {
                    program: value.clone(),
                })
            }
            "--endpoint" => {
                kind = Some(AgentKind::OpenAiCompatible {
                    endpoint: value.clone(),
                })
            }
            "--model" => {
                let (id, grade) = value
                    .rsplit_once('=')
                    .ok_or("a model is written <id>=<T0..T3>")?;
                let grade: Grade = grade_from(grade).ok_or("a grade is T0, T1, T2 or T3")?;
                models.push(CatalogModel {
                    id: id.to_owned(),
                    grade,
                });
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(AgentEntry {
        name,
        kind: kind.ok_or("give --program or --endpoint")?,
        models,
        personal: true,
    })
}

fn state_directory() -> Result<PathBuf, String> {
    kaisa_sys::state_location(|name| std::env::var_os(name))
        .map(|location| location.directory)
        .map_err(|error| error.to_string())
}

/// Runs `kaisa agent <args>`.
pub(crate) fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> io::Result<u8> {
    let state = match state_directory() {
        Ok(state) => state,
        Err(error) => {
            writeln!(err, "kaisa: {error}")?;
            return Ok(crate::EXIT_FAILURE);
        }
    };
    match args.first().map(String::as_str) {
        Some("add") => {
            let entry = match parse_add(&args[1..]) {
                Ok(entry) => entry,
                Err(problem) => {
                    writeln!(err, "kaisa agent add: {problem}")?;
                    return Ok(crate::EXIT_USAGE);
                }
            };
            let mut catalog = load(&state)?;
            let name: String = entry.name.clone();
            if let Err(refused) = catalog.add(entry) {
                writeln!(err, "kaisa agent add: {refused}")?;
                return Ok(crate::EXIT_FAILURE);
            }
            save(&state, &catalog)?;
            writeln!(out, "added personal agent {name}")?;
            Ok(crate::EXIT_SUCCESS)
        }
        Some("list") => {
            for entry in load(&state)?.entries() {
                let models: Vec<String> = entry
                    .models
                    .iter()
                    .map(|model| format!("{}={:?}", model.id, model.grade))
                    .collect();
                writeln!(out, "{} {}", entry.name, models.join(" "))?;
            }
            Ok(crate::EXIT_SUCCESS)
        }
        _ => {
            writeln!(
                err,
                "usage: kaisa agent add <name> (--program <path> | --endpoint <url>) --model <id>=<T0..T3>\n       kaisa agent list"
            )?;
            Ok(crate::EXIT_USAGE)
        }
    }
}
