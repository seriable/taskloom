//! System prompts by key, overridable without a rebuild.
//!
//! Every task names its system prompt with a stable key and passes the
//! built-in text ([`PromptOverrides::prompt`]). A deployment may replace any
//! of them, for example from a JSON file `{ "critic.engagement": "…" }`
//! ([`PromptOverrides::from_json`]).
//!
//! [`PromptOverrides::fingerprint`] identifies the overrides in effect, so a
//! cache of results can tell results computed under other prompts. With no
//! override it is `None`: the built-in prompts are versioned with the code.

use std::borrow::Cow;
use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

/// Prompt texts that replace built-in system prompts, by key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptOverrides {
    prompts: BTreeMap<String, String>,
    fingerprint: Option<String>,
}

impl PromptOverrides {
    /// No override.
    pub fn none() -> Self {
        Self::default()
    }

    /// The overrides of `prompts`; keys starting with `_` (comments) and
    /// blank texts are no override.
    pub fn from_map(prompts: BTreeMap<String, String>) -> Self {
        let prompts = prompts
            .into_iter()
            .filter(|(key, text)| !key.starts_with('_') && !text.trim().is_empty())
            .collect::<BTreeMap<_, _>>();
        let fingerprint = (!prompts.is_empty()).then(|| {
            let mut hasher = Sha256::new();
            for (key, text) in &prompts {
                hasher.update(key.as_bytes());
                hasher.update([0]);
                hasher.update(text.as_bytes());
                hasher.update([0]);
            }
            hex::encode(hasher.finalize())
        });
        Self {
            prompts,
            fingerprint,
        }
    }

    /// The overrides of a JSON object of strings.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str::<BTreeMap<String, String>>(json).map(Self::from_map)
    }

    /// The system prompt `key`: its override when one is configured, else
    /// `built_in`.
    pub fn prompt<'a>(&self, key: &str, built_in: &'a str) -> Cow<'a, str> {
        match self.prompts.get(key) {
            Some(text) => Cow::Owned(text.clone()),
            None => Cow::Borrowed(built_in),
        }
    }

    /// Identity of the overrides; `None` when there is none.
    pub fn fingerprint(&self) -> Option<&str> {
        self.fingerprint.as_deref()
    }

    /// The overridden keys.
    pub fn keys(&self) -> Vec<&str> {
        self.prompts.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_blank_texts_are_no_override() {
        let overrides = PromptOverrides::from_map(BTreeMap::from([
            ("_comment".to_string(), "notes".to_string()),
            ("blank".to_string(), "  ".to_string()),
        ]));
        assert!(overrides.keys().is_empty());
        assert_eq!(overrides.fingerprint(), None);
    }

    #[test]
    fn the_fingerprint_follows_the_texts() {
        let one = PromptOverrides::from_json(r#"{"a":"x"}"#).unwrap();
        let two = PromptOverrides::from_json(r#"{"a":"y"}"#).unwrap();
        assert!(one.fingerprint().is_some());
        assert_ne!(one.fingerprint(), two.fingerprint());
        assert_eq!(one.prompt("a", "built-in"), "x");
        assert_eq!(one.prompt("b", "built-in"), "built-in");
    }
}
