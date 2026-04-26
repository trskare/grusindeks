//! Language selection for human-rendered output.
//!
//! All user-facing strings (score labels, penalty messages, compass
//! abbreviations, day labels) are produced through helpers that take a
//! `Language`. Norwegian is the default; Swedish is supported via a
//! config flag. The brand name "Grusindeks" stays untranslated.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Norwegian,
    Swedish,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_norwegian() {
        assert_eq!(Language::default(), Language::Norwegian);
    }

    #[test]
    fn serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Language::Norwegian).unwrap(),
            "\"norwegian\""
        );
        assert_eq!(
            serde_json::to_string(&Language::Swedish).unwrap(),
            "\"swedish\""
        );
    }

    #[test]
    fn deserializes_lowercase() {
        let no: Language = serde_json::from_str("\"norwegian\"").unwrap();
        let se: Language = serde_json::from_str("\"swedish\"").unwrap();
        assert_eq!(no, Language::Norwegian);
        assert_eq!(se, Language::Swedish);
    }
}
