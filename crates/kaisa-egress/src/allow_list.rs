//! The domains one attempt may reach.

/// Host names an attempt may reach: exact names, or `.suffix` entries that match the name and
/// every subdomain of it. Matching ignores ASCII case and a trailing dot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AllowList {
    entries: Vec<String>,
}

fn normalise(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

impl AllowList {
    /// An allow-list of `entries`.
    #[must_use]
    pub fn new(entries: &[&str]) -> Self {
        Self {
            entries: entries.iter().map(|entry| normalise(entry)).collect(),
        }
    }

    /// True if `host` may be reached.
    #[must_use]
    pub fn permits(&self, host: &str) -> bool {
        let host: String = normalise(host);
        self.entries
            .iter()
            .any(|entry| match entry.strip_prefix('.') {
                Some(suffix) => host == suffix || host.ends_with(entry.as_str()),
                None => host == *entry,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::AllowList;

    #[test]
    fn a_suffix_entry_covers_subdomains_but_not_look_alikes() {
        let registries: AllowList = AllowList::new(&[".npmjs.org"]);

        assert_eq!(
            (
                registries.permits("registry.npmjs.org"),
                registries.permits("npmjs.org"),
                registries.permits("evilnpmjs.org")
            ),
            (true, true, false),
            "a look-alike domain must never ride on an allowed suffix"
        );
    }
}
