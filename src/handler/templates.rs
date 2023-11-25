use askama::Template;
use fluent_templates::{LanguageIdentifier, Loader};
use ulid::Ulid;

use crate::internationalization::LOCALES;

use super::auth::FediverseUser;

mod filters;

fn t(language: &LanguageIdentifier, text_id: &str) -> String {
    LOCALES
        .lookup(language, text_id)
        .unwrap_or_else(|| format!("t({})", text_id))
}

// fn ta<'a>(
//     language: &'a LanguageIdentifier,
//     text_id: &'a str,
//     args: &'a [(&'a str, impl Into<FluentValue<'a>> + Clone + 'a)],
// ) -> String {
//     let args = args
//         .iter()
//         .map(|(key, value)| (key.to_string(), value.clone().into()))
//         .collect::<HashMap<_, _>>();
//     LOCALES
//         .lookup_with_args(language, text_id, &args)
//         .unwrap_or_else(|| format!("ta({})", text_id))
// }

pub struct TemplateError {
    pub summary: String,
    pub detail: Option<String>,
}

#[derive(Template)]
#[template(path = "index-login.html")]
pub struct IndexLoginTemplate {
    pub language: LanguageIdentifier,
    pub user: FediverseUser,
    pub is_bulk_selected: bool,
    pub quotes: Vec<(Ulid, String, bool)>,
    pub quote_input: String,
    pub quote_bulk_input: String,
    pub quote_error: Option<TemplateError>,
    pub cron_input: String,
    pub cron_error: Option<TemplateError>,
    pub dedup_duration_hours: u32,
    pub suspend_schedule: bool,
}

#[derive(Template)]
#[template(path = "index-logout.html")]
pub struct IndexLogoutTemplate {
    pub language: LanguageIdentifier,
    pub domain: String,
    pub domain_error: Option<TemplateError>,
}

#[derive(Template)]
#[template(path = "auth-failed.html")]
pub struct AuthFailedTemplate {
    pub language: LanguageIdentifier,
    pub error: String,
}
