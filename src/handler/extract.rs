use std::str::FromStr;

use async_trait::async_trait;
use axum::{extract::FromRequestParts, http::request::Parts};
use fluent_templates::LanguageIdentifier;
use serde::Deserialize;

pub struct Language(pub LanguageIdentifier);

#[derive(Deserialize)]
struct LanguageQuery {
    lang: String,
}

#[async_trait]
impl<S> FromRequestParts<S> for Language
where
    S: Send + Sync,
{
    type Rejection = ();

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Ok(axum::extract::Query(query)) =
            axum::extract::Query::<LanguageQuery>::from_request_parts(parts, state).await
        {
            if let Ok(language_identifier) = LanguageIdentifier::from_str(&query.lang) {
                return Ok(Self(language_identifier));
            }
        }
        if let Some(header) = parts.headers.get("accept-language") {
            if let Ok(header) = header.to_str() {
                if let Ok(language_identifier) = LanguageIdentifier::from_str(header) {
                    return Ok(Self(language_identifier));
                }
            }
        }
        Ok(Self(LanguageIdentifier::from_str("ko-KR").unwrap()))
    }
}
