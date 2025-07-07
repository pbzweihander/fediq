use std::fmt;

use askama::Template;
use axum::{
    extract::{FromRequestParts, Path, Query},
    response::{IntoResponseParts, Redirect},
    routing, RequestPartsExt, Router,
};
use axum_extra::{headers, TypedHeader};
use http::{header, HeaderName, HeaderValue, StatusCode};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use url::Url;

use crate::{
    api::fediverse::{login_mastodon, login_misskey},
    config::CONFIG,
};

use super::{extract::Language, templates::AuthFailedTemplate};

const SESSION_COOKIE_KEY: &str = "SESSION";
const SESSION_EXP_DURATION: Duration = Duration::days(1);

#[derive(Deserialize, Serialize)]
pub struct FediverseUser {
    pub domain: String,
    pub handle: String,
    pub name: Option<String>,
    pub avatar_url: Option<Url>,
    pub access_token: String,
    pub software: String,
    exp: i64,
}

impl fmt::Display for FediverseUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = &self.name {
            write!(f, "{} ({}@{})", name, self.handle, self.domain)
        } else {
            write!(f, "{}@{}", self.handle, self.domain)
        }
    }
}

impl FediverseUser {
    pub fn new(
        domain: String,
        handle: String,
        name: Option<String>,
        avatar_url: Option<Url>,
        access_token: String,
        software: String,
    ) -> Self {
        let now = OffsetDateTime::now_utc();
        let exp = (now + SESSION_EXP_DURATION).unix_timestamp();

        Self {
            domain,
            handle,
            name,
            avatar_url,
            access_token,
            software,
            exp,
        }
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.handle)
    }
}

impl<S> FromRequestParts<S> for FediverseUser
where
    S: Send + Sync,
{
    type Rejection = ();

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let cookies = parts
            .extract::<TypedHeader<headers::Cookie>>()
            .await
            .map_err(|_| ())?;
        let cookie = cookies.get(SESSION_COOKIE_KEY).ok_or(())?;

        let mut jwt_validation = jsonwebtoken::Validation::default();
        jwt_validation.validate_exp = true;
        let userdata =
            jsonwebtoken::decode::<FediverseUser>(cookie, &CONFIG.jwt_secret.1, &jwt_validation)
                .map_err(|_| ())?;

        Ok(userdata.claims)
    }
}

impl IntoResponseParts for FediverseUser {
    type Error = (StatusCode, String);

    fn into_response_parts(
        self,
        mut res: axum::response::ResponseParts,
    ) -> Result<axum::response::ResponseParts, Self::Error> {
        let jwt_token = jsonwebtoken::encode(&Default::default(), &self, &CONFIG.jwt_secret.0)
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:?}")))?;

        res.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&format!(
                "{SESSION_COOKIE_KEY}={jwt_token}; SameSite=Lax; Path=/"
            ))
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:?}")))?,
        );
        Ok(res)
    }
}

pub fn create_router() -> Router {
    Router::new()
        .route("/logout", routing::get(get_logout))
        .route(
            "/callback/mastodon/{domain}",
            routing::get(get_callback_mastodon),
        )
        .route(
            "/callback/misskey/{domain}",
            routing::get(get_callback_misskey),
        )
}

async fn get_logout() -> ([(HeaderName, String); 1], Redirect) {
    (
        [(
            header::SET_COOKIE,
            format!("{SESSION_COOKIE_KEY}=; SameSite=Lax; Path=/"),
        )],
        Redirect::to("/"),
    )
}

#[derive(Deserialize)]
struct MastodonCallbackQuery {
    code: String,
}

async fn get_callback_mastodon(
    Language(language): Language,
    Query(query): Query<MastodonCallbackQuery>,
    Path(domain): Path<String>,
) -> Result<(FediverseUser, Redirect), String> {
    match login_mastodon(&domain, &query.code).await {
        Ok(user) => Ok((user, Redirect::to("/"))),
        Err(error) => Err(AuthFailedTemplate {
            language,
            error: format!("{error:?}"),
        }
        .render()
        .unwrap()),
    }
}

#[derive(Deserialize)]
struct MisskeyCallbackQuery {
    token: String,
}

async fn get_callback_misskey(
    Language(language): Language,
    Query(query): Query<MisskeyCallbackQuery>,
    Path(domain): Path<String>,
) -> Result<(FediverseUser, Redirect), String> {
    match login_misskey(&domain, &query.token).await {
        Ok(user) => Ok((user, Redirect::to("/"))),
        Err(error) => Err(AuthFailedTemplate {
            language,
            error: format!("{error:?}"),
        }
        .render()
        .unwrap()),
    }
}
