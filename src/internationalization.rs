use fluent_templates::{LanguageIdentifier, Loader};

fluent_templates::static_loader! {
    pub static LOCALES = {
        locales: "locales",
        fallback_language: "ko-KR",
    };
}

pub fn t(lang: &LanguageIdentifier, text_id: &str) -> String {
    LOCALES
        .lookup(lang, text_id)
        .unwrap_or_else(|| format!("t({})", text_id))
}
