//! Locale resolution and application of the configured language.

use crate::config::Language;

/// Catalogues shipped with the binary.
pub const CATALOGUES: [&str; 2] = ["en", "tr"];

fn matches_turkish(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("tr") || lower.split(['_', '.', '-']).next() == Some("tr")
}

/// True when the process locale asks for Turkish.
pub fn system_is_turkish() -> bool {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(var)
            && !value.trim().is_empty()
        {
            return matches_turkish(&value);
        }
    }
    false
}

/// Concrete catalogue id for a configured language.
pub fn resolve(language: Language) -> &'static str {
    match language {
        Language::En => "en",
        Language::Tr => "tr",
        Language::System => {
            if system_is_turkish() {
                "tr"
            } else {
                "en"
            }
        }
    }
}

/// Apply the language to the runtime catalogues for this process.
pub fn apply(language: Language) {
    let locale = resolve(language);
    rust_i18n::set_locale(locale);
    tracing::info!(locale, requested = language.id(), "language applied");
}

/// Locale id sent to the web remote (always a concrete catalogue).
pub fn remote_locale() -> String {
    rust_i18n::locale().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_and_turkish_resolve() {
        assert_eq!(resolve(Language::En), "en");
        assert_eq!(resolve(Language::Tr), "tr");
        // System always resolves to a shipped catalogue.
        assert!(CATALOGUES.contains(&resolve(Language::System)));
    }

    #[test]
    fn turkish_locales_are_detected() {
        assert!(matches_turkish("tr_TR.UTF-8"));
        assert!(matches_turkish("tr"));
        assert!(!matches_turkish("en_US.UTF-8"));
        assert!(!matches_turkish("C.UTF-8"));
    }
}
