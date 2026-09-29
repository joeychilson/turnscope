//! Which model a usage record names.

use std::fmt;

/// The name a model is counted under.
///
/// Agents name one model in several ways. OpenRouter records
/// `google/gemini-3.8-flash` where Google's own API records `gemini-3.8-flash`,
/// and Claude Code's own totals record `claude-opus-5[1m]` for the
/// one-million-token context of `claude-opus-5`. The key drops any path before
/// the name and any bracketed variant after it, so each of those is one model.
/// The id as recorded is kept beside the key wherever it is stored, because a
/// price is looked up by the provider's own id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelKey(String);

impl ModelKey {
    /// The key a recorded model id is counted under.
    pub fn of(recorded: &str) -> ModelKey {
        let name = recorded.trim();
        let name = name.rsplit_once('/').map_or(name, |(_, name)| name);
        let name = match name
            .strip_suffix(']')
            .and_then(|rest| rest.rsplit_once('['))
        {
            Some((base, _)) if !base.is_empty() => base,
            _ => name,
        };
        ModelKey(name.to_owned())
    }

    /// A key [`ModelKey::of`] made already, as the cache stores one. Made
    /// again, a key could change: `m[a][b]` is counted under `m[a]`, and
    /// that under `m`.
    pub(crate) fn stored(key: String) -> ModelKey {
        ModelKey(key)
    }

    /// The key as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A model this Mac has used, as the interface names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    /// The key its usage is counted under.
    pub key: ModelKey,
    /// What the catalog calls it, such as `Claude Opus 5.5`; `None` for a
    /// model the catalog doesn't list, which is then known by its key.
    pub name: Option<String>,
}

impl fmt::Display for ModelKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::ModelKey;

    #[test]
    fn a_model_is_counted_under_its_bare_name() {
        assert_eq!(
            ModelKey::of("google/gemini-3.8-flash").as_str(),
            "gemini-3.8-flash"
        );
        assert_eq!(ModelKey::of("claude-opus-5[1m]").as_str(), "claude-opus-5");
        assert_eq!(
            ModelKey::of("claude-haiku-4-5-20251001").as_str(),
            "claude-haiku-4-5-20251001"
        );
    }

    #[test]
    fn a_name_that_is_only_brackets_is_kept_whole() {
        assert_eq!(ModelKey::of("[1m]").as_str(), "[1m]");
        assert_eq!(ModelKey::of("<synthetic>").as_str(), "<synthetic>");
    }
}
