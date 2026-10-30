//! Validated agent catalog and deterministic model choices (architecture §5.6 and §5.10).

use std::fmt;

use crate::grade::Grade;

/// The transport used to invoke an agent or a tool-less model server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentKind {
    /// An agent CLI whose file access is mediated by the broker.
    CliAgent {
        /// The program to execute.
        program: String,
    },
    /// A tool-less server implementing the OpenAI-compatible API.
    OpenAiCompatible {
        /// The configured server URL receiving prompts.
        endpoint: String,
    },
}

/// A model offered by an agent and its routing grade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    /// The model identifier passed to the agent or server.
    pub id: String,
    /// The strength of work this model can handle.
    pub grade: Grade,
}

/// An agent registration, shared or owned by the current person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEntry {
    /// A unique lowercase ASCII name containing only letters, digits and hyphens.
    pub name: String,
    /// How Kaisa invokes this agent.
    pub kind: AgentKind,
    /// The models available through this agent.
    pub models: Vec<CatalogModel>,
    /// Whether this registration belongs to the current person.
    pub personal: bool,
}

/// Why an agent registration cannot be added to the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogError {
    /// The name is empty or contains bytes outside `[a-z0-9-]`.
    InvalidName,
    /// Another registration already uses the name.
    DuplicateName,
    /// The registration offers no models.
    NoModels,
    /// A model identifier is empty.
    EmptyModelId,
    /// The endpoint is neither HTTPS nor HTTP on an allowed loopback host.
    UnsafeEndpoint,
    /// A personal registration uses a name reserved for a built-in agent.
    ReservedName,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidName => "an agent name must be nonempty and contain only a-z, 0-9 and -",
            Self::DuplicateName => "an agent with this name is already registered",
            Self::NoModels => "an agent must offer at least one model",
            Self::EmptyModelId => "a model identifier must not be empty",
            Self::UnsafeEndpoint => {
                "a model endpoint requires HTTPS or HTTP on 127.0.0.1 or localhost"
            }
            Self::ReservedName => "a personal agent cannot use the built-in name claude or codex",
        })
    }
}

impl std::error::Error for CatalogError {}

/// Validated registrations used by the router; personal additions need no admin authority.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    entries: Vec<AgentEntry>,
}

impl Catalog {
    /// Creates an empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates and adds a registration without changing existing entries on failure.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] for an invalid or duplicate name, missing models or model
    /// identifiers, an unsafe endpoint, or a personal registration using a built-in name.
    pub fn add(&mut self, entry: AgentEntry) -> Result<(), CatalogError> {
        if entry.name.is_empty()
            || !entry
                .name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(CatalogError::InvalidName);
        }
        if entry.personal && matches!(entry.name.as_str(), "claude" | "codex") {
            return Err(CatalogError::ReservedName);
        }
        if self.entries.iter().any(|known| known.name == entry.name) {
            return Err(CatalogError::DuplicateName);
        }
        if entry.models.is_empty() {
            return Err(CatalogError::NoModels);
        }
        if entry.models.iter().any(|model| model.id.is_empty()) {
            return Err(CatalogError::EmptyModelId);
        }
        if let AgentKind::OpenAiCompatible { endpoint } = &entry.kind
            && !endpoint_is_safe(endpoint)
        {
            return Err(CatalogError::UnsafeEndpoint);
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Returns registrations in insertion order without permitting validation to be bypassed.
    #[must_use]
    pub fn entries(&self) -> &[AgentEntry] {
        &self.entries
    }

    /// Returns eligible models sorted by grade, agent name and model identifier.
    ///
    /// The lowest eligible grade comes first; models below the requested grade are excluded.
    #[must_use]
    pub fn models_at_or_above(&self, grade: Grade) -> Vec<(&AgentEntry, &CatalogModel)> {
        let mut eligible: Vec<(&AgentEntry, &CatalogModel)> = self
            .entries
            .iter()
            .flat_map(|entry| entry.models.iter().map(move |model| (entry, model)))
            .filter(|(_, model)| model.grade >= grade)
            .collect();
        eligible.sort_by(|(left_agent, left_model), (right_agent, right_model)| {
            (left_model.grade, &left_agent.name, &left_model.id).cmp(&(
                right_model.grade,
                &right_agent.name,
                &right_model.id,
            ))
        });
        eligible
    }

    /// Encodes the catalog as sorted `agent.<name>.<field>=value` lines with final newlines.
    ///
    /// Fields are sorted by key; model lists are sorted by identifier and grade. Model entries
    /// use `id:grade` separated by commas, with stable grade names `T0` through `T3`.
    /// Text bytes outside ASCII letters, digits, `-`, `.`, `_` and `~` are percent-encoded
    /// with uppercase hex, preventing delimiters from colliding with field contents.
    /// An empty catalog encodes as empty text. Callers may hash these UTF-8 bytes.
    #[must_use]
    pub fn canonical_text(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for entry in &self.entries {
            let prefix: String = format!("agent.{}", entry.name);
            let mut models: Vec<&CatalogModel> = entry.models.iter().collect();
            models.sort_by(|left, right| (&left.id, left.grade).cmp(&(&right.id, right.grade)));
            let models: String = models
                .into_iter()
                .map(|model| format!("{}:{}", encode_text(&model.id), grade_name(model.grade)))
                .collect::<Vec<_>>()
                .join(",");
            lines.push(format!("{prefix}.models={models}\n"));
            lines.push(format!("{prefix}.personal={}\n", entry.personal));
            match &entry.kind {
                AgentKind::CliAgent { program } => {
                    lines.push(format!("{prefix}.kind=cli-agent\n"));
                    lines.push(format!("{prefix}.program={}\n", encode_text(program)));
                }
                AgentKind::OpenAiCompatible { endpoint } => {
                    lines.push(format!("{prefix}.endpoint={}\n", encode_text(endpoint)));
                    lines.push(format!("{prefix}.kind=openai-compatible\n"));
                }
            }
        }
        lines.sort_unstable();
        lines.concat()
    }
}

fn endpoint_is_safe(endpoint: &str) -> bool {
    if endpoint
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace() || byte == b'\\')
    {
        return false;
    }
    let (is_https, remainder): (bool, &str) = if let Some(rest) = endpoint.strip_prefix("https://")
    {
        (true, rest)
    } else if let Some(rest) = endpoint.strip_prefix("http://") {
        (false, rest)
    } else {
        return false;
    };
    let authority: &str = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    if is_https {
        return true;
    }
    let host: &str = if let Some((host, port)) = authority.split_once(':') {
        if port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().is_err()
        {
            return false;
        }
        host
    } else {
        authority
    };
    matches!(host, "127.0.0.1" | "localhost")
}

fn grade_name(grade: Grade) -> &'static str {
    match grade {
        Grade::T0 => "T0",
        Grade::T1 => "T1",
        Grade::T2 => "T2",
        Grade::T3 => "T3",
    }
}

fn encode_text(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded: String = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{AgentEntry, AgentKind, Catalog, CatalogError, CatalogModel};
    use crate::grade::Grade;

    fn local_agent(name: &str) -> AgentEntry {
        AgentEntry {
            name: name.into(),
            kind: AgentKind::OpenAiCompatible {
                endpoint: "http://localhost:11434/v1".into(),
            },
            models: vec![CatalogModel {
                id: "local-light".into(),
                grade: Grade::T1,
            }],
            personal: true,
        }
    }

    fn catalog_with(entries: Vec<AgentEntry>) -> Catalog {
        let mut catalog: Catalog = Catalog::new();
        for entry in entries {
            catalog
                .add(entry)
                .expect("the fixture registration is valid");
        }
        catalog
    }

    #[test]
    fn a_personal_agent_is_added_without_an_admin() {
        let personal_agent: AgentEntry = local_agent("ollama");
        let mut catalog: Catalog = Catalog::default();

        assert_eq!(
            catalog.add(personal_agent.clone()),
            Ok(()),
            "F37 personal registration needs no admin authority"
        );
        assert_eq!(
            catalog.entries(),
            &[personal_agent],
            "the personal model remains available to routing"
        );
    }

    #[test]
    fn an_unencrypted_remote_endpoint_is_refused() {
        for endpoint in [
            "http://models.example/v1",
            "http://localhost.example/v1",
            "http://127.0.0.1.example/v1",
            "http://localhost@models.example/v1",
            "http://localhost\\@models.example/v1",
            "http://localhost:bad/v1",
            "https://",
            "file:///models",
        ] {
            let mut agent: AgentEntry = local_agent("ollama");
            agent.kind = AgentKind::OpenAiCompatible {
                endpoint: endpoint.into(),
            };
            let mut catalog: Catalog = Catalog::new();

            assert_eq!(
                catalog.add(agent),
                Err(CatalogError::UnsafeEndpoint),
                "unsafe endpoint {endpoint:?} must not receive prompts"
            );
            assert!(
                catalog.entries().is_empty(),
                "a refused endpoint must not alter the catalog"
            );
        }
    }

    #[test]
    fn loopback_http_and_remote_https_endpoints_are_allowed() {
        for endpoint in [
            "http://127.0.0.1",
            "http://127.0.0.1:8080/v1",
            "http://localhost",
            "http://localhost:11434/v1",
            "https://models.example/v1",
        ] {
            let mut agent: AgentEntry = local_agent("ollama");
            agent.kind = AgentKind::OpenAiCompatible {
                endpoint: endpoint.into(),
            };

            assert_eq!(
                Catalog::new().add(agent),
                Ok(()),
                "the supported endpoint {endpoint:?} can serve configured models"
            );
        }
    }

    #[test]
    fn a_personal_entry_cannot_take_a_built_in_name() {
        for name in ["claude", "codex"] {
            let mut built_in: AgentEntry = local_agent(name);
            assert_eq!(
                Catalog::new().add(built_in.clone()),
                Err(CatalogError::ReservedName),
                "personal entries cannot impersonate {name}"
            );
            built_in.personal = false;
            assert_eq!(
                Catalog::new().add(built_in),
                Ok(()),
                "shared built-in registrations retain their reserved name"
            );
        }
    }

    #[test]
    fn invalid_names_and_missing_models_are_refused() {
        for name in [
            "",
            "Ollama",
            "local_model",
            "local.model",
            "local model",
            "모델",
        ] {
            assert_eq!(
                Catalog::new().add(local_agent(name)),
                Err(CatalogError::InvalidName),
                "agent name {name:?} is outside the catalog alphabet"
            );
        }
        let mut agent: AgentEntry = local_agent("ollama");
        agent.models.clear();
        assert_eq!(
            Catalog::new().add(agent.clone()),
            Err(CatalogError::NoModels),
            "routing needs an offered model"
        );
        agent.models.push(CatalogModel {
            id: String::new(),
            grade: Grade::T1,
        });
        assert_eq!(
            Catalog::new().add(agent),
            Err(CatalogError::EmptyModelId),
            "an invocation needs a nonempty model identifier"
        );
    }

    #[test]
    fn a_duplicate_name_is_refused_without_replacing_the_registration() {
        let agent: AgentEntry = local_agent("ollama");
        let mut catalog: Catalog = catalog_with(vec![agent.clone()]);
        let mut duplicate: AgentEntry = agent.clone();
        duplicate.models.clear();

        assert_eq!(
            catalog.add(duplicate),
            Err(CatalogError::DuplicateName),
            "registration cannot replace an existing name"
        );
        assert_eq!(
            catalog.entries(),
            &[agent],
            "rejection preserves the original registration"
        );
    }

    #[test]
    fn the_cheapest_eligible_model_comes_first() {
        let mut alpha: AgentEntry = local_agent("alpha");
        alpha.models = vec![
            CatalogModel {
                id: "heavy".into(),
                grade: Grade::T3,
            },
            CatalogModel {
                id: "standard-z".into(),
                grade: Grade::T2,
            },
            CatalogModel {
                id: "light".into(),
                grade: Grade::T1,
            },
            CatalogModel {
                id: "standard-a".into(),
                grade: Grade::T2,
            },
        ];
        let mut beta: AgentEntry = local_agent("beta");
        beta.models = vec![CatalogModel {
            id: "standard".into(),
            grade: Grade::T2,
        }];
        let catalog: Catalog = catalog_with(vec![beta, alpha]);
        let routes: Vec<(&str, &str, Grade)> = catalog
            .models_at_or_above(Grade::T2)
            .iter()
            .map(|(agent, model)| (agent.name.as_str(), model.id.as_str(), model.grade))
            .collect();

        assert_eq!(
            routes,
            vec![
                ("alpha", "standard-a", Grade::T2),
                ("alpha", "standard-z", Grade::T2),
                ("beta", "standard", Grade::T2),
                ("alpha", "heavy", Grade::T3)
            ],
            "routing filters weaker models and orders ties by agent then model"
        );
    }

    #[test]
    fn the_canonical_text_does_not_depend_on_insertion_order() {
        let mut local: AgentEntry = local_agent("ollama");
        local.models.push(CatalogModel {
            id: "local-heavy".into(),
            grade: Grade::T3,
        });
        let mut cli: AgentEntry = local_agent("hermes");
        cli.kind = AgentKind::CliAgent {
            program: "hermes".into(),
        };
        let original: Catalog = catalog_with(vec![local.clone(), cli.clone()]);
        local.models.reverse();
        let reordered: Catalog = catalog_with(vec![cli, local]);

        assert_eq!(
            original.canonical_text(),
            reordered.canonical_text(),
            "registration and model order do not change the catalog digest input"
        );
    }

    #[test]
    fn the_canonical_text_contains_every_field_in_key_order() {
        let mut cli: AgentEntry = local_agent("hermes");
        cli.kind = AgentKind::CliAgent {
            program: "hermes --stdio\n%".into(),
        };
        cli.models = vec![CatalogModel {
            id: "model:one,two".into(),
            grade: Grade::T2,
        }];
        let catalog: Catalog = catalog_with(vec![cli, local_agent("ollama")]);

        assert_eq!(
            catalog.canonical_text(),
            concat!(
                "agent.hermes.kind=cli-agent\n",
                "agent.hermes.models=model%3Aone%2Ctwo:T2\n",
                "agent.hermes.personal=true\n",
                "agent.hermes.program=hermes%20--stdio%0A%25\n",
                "agent.ollama.endpoint=http%3A%2F%2Flocalhost%3A11434%2Fv1\n",
                "agent.ollama.kind=openai-compatible\n",
                "agent.ollama.models=local-light:T1\n",
                "agent.ollama.personal=true\n",
            ),
            "all routing and ownership fields have stable, escaped spellings"
        );
        assert_eq!(
            Catalog::new().canonical_text(),
            "",
            "an empty catalog has no field lines"
        );
    }

    #[test]
    fn model_delimiters_cannot_collide_with_a_different_model_list() {
        let mut single_model: AgentEntry = local_agent("ollama");
        single_model.models = vec![CatalogModel {
            id: "a:T1,b".into(),
            grade: Grade::T2,
        }];
        let mut separate_models: AgentEntry = single_model.clone();
        separate_models.models = vec![
            CatalogModel {
                id: "a".into(),
                grade: Grade::T1,
            },
            CatalogModel {
                id: "b".into(),
                grade: Grade::T2,
            },
        ];

        assert_ne!(
            catalog_with(vec![single_model]).canonical_text(),
            catalog_with(vec![separate_models]).canonical_text(),
            "model punctuation cannot hide a different set of routing options"
        );
    }
}
