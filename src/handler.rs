pub mod auth;
mod extract;
mod templates;

use axum::{headers, response::Redirect, routing, Form, Router, TypedHeader};
use serde::Deserialize;
use ulid::Ulid;

use crate::{
    api::{
        fediverse::get_auth_redirect_url,
        kube::{add_quotes, delete_quote, load_cronjob, load_quotes, save_cronjob},
    },
    internationalization::t,
};

use self::{
    auth::FediverseUser,
    extract::Language,
    templates::{IndexLoginTemplate, IndexLogoutTemplate, TemplateError},
};

pub fn create_router() -> Router {
    let auth = auth::create_router();

    Router::new()
        .route("/index.css", routing::get(get_index_css))
        .route("/healthz", routing::get(get_healthz))
        .route("/", routing::get(get_index).post(post_index))
        .nest("/auth", auth)
}

async fn get_index_css() -> (TypedHeader<headers::ContentType>, &'static [u8]) {
    (
        TypedHeader(headers::ContentType::from(mime::TEXT_CSS)),
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/dist/index.css")),
    )
}

async fn get_healthz() -> &'static str {
    "ok"
}

async fn get_index(
    Language(language): Language,
    user: Option<FediverseUser>,
) -> Result<IndexLoginTemplate, IndexLogoutTemplate> {
    if let Some(user) = user {
        let quotes = load_quotes(&user.domain, &user.handle)
            .await
            .unwrap_or_else(|error| {
                tracing::error!(?error, "failed to load quotes");
                Vec::new()
            });
        let (cron_input, dedup_duration_minutes, suspend_schedule) =
            load_cronjob(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to load schedule");
                    (String::new(), 0, false)
                });

        Ok(IndexLoginTemplate {
            language,
            user,
            quotes,
            is_bulk_selected: false,
            quote_input: String::new(),
            quote_bulk_input: String::new(),
            quote_error: None,
            cron_input,
            cron_error: None,
            dedup_duration_minutes,
            suspend_schedule,
        })
    } else {
        Err(IndexLogoutTemplate {
            language,
            domain: String::new(),
            domain_error: None,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", tag = "add_quote_mode")]
enum AddQuote {
    OneByOne {
        #[serde(default)]
        quote: String,
    },
    Bulk {
        #[serde(default)]
        quote_bulk: String,
    },
}

impl AddQuote {
    fn is_empty(&self) -> bool {
        match self {
            Self::OneByOne { quote } => quote.is_empty(),
            Self::Bulk { quote_bulk } => quote_bulk.is_empty(),
        }
    }

    fn is_bulk(&self) -> bool {
        matches!(self, Self::Bulk { quote_bulk: _ })
    }

    fn as_one_by_one(&self) -> String {
        match self {
            Self::OneByOne { quote } => quote.clone(),
            Self::Bulk { quote_bulk: _ } => String::new(),
        }
    }

    fn as_bulk(&self) -> String {
        match self {
            Self::OneByOne { quote: _ } => String::new(),
            Self::Bulk { quote_bulk } => quote_bulk.clone(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum PostIndexReq {
    Login {
        #[serde(default)]
        domain: String,
    },
    AddQuote(AddQuote),
    ConfigureSchedule {
        cron: String,
        #[serde(default)]
        suspend: String,
        #[serde(default)]
        dedup_duration_minutes: String,
    },
    DeleteQuote {
        quote_id: Ulid,
    },
}

async fn post_index(
    user: Option<FediverseUser>,
    Language(language): Language,
    Form(req): Form<PostIndexReq>,
) -> Result<IndexLoginTemplate, Result<Redirect, IndexLogoutTemplate>> {
    match (user, req) {
        (_, PostIndexReq::Login { domain }) => {
            if domain.is_empty() {
                return Err(Err(IndexLogoutTemplate {
                    domain,
                    domain_error: Some(TemplateError {
                        summary: t(&language, "value-cannot-empty"),
                        detail: None,
                    }),
                    language,
                }));
            }

            match get_auth_redirect_url(&domain).await {
                Ok(redirect_url) => Err(Ok(Redirect::to(redirect_url.as_str()))),
                Err(error) => Err(Err(IndexLogoutTemplate {
                    domain,
                    domain_error: Some(TemplateError {
                        summary: t(&language, "login-error"),
                        detail: Some(format!("{error:?}")),
                    }),
                    language,
                })),
            }
        }
        (Some(user), PostIndexReq::AddQuote(req)) => {
            if req.is_empty() {
                let quotes = load_quotes(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load quotes");
                        Vec::new()
                    });
                let (cron_input, dedup_duration_minutes, suspend_schedule) =
                    load_cronjob(&user.domain, &user.handle)
                        .await
                        .unwrap_or_else(|error| {
                            tracing::error!(?error, "failed to load schedule");
                            (String::new(), 0, false)
                        });

                Ok(IndexLoginTemplate {
                    user,
                    quotes,
                    is_bulk_selected: req.is_bulk(),
                    quote_input: req.as_one_by_one(),
                    quote_bulk_input: req.as_bulk(),
                    quote_error: Some(TemplateError {
                        summary: t(&language, "value-cannot-empty"),
                        detail: None,
                    }),
                    cron_input,
                    cron_error: None,
                    dedup_duration_minutes,
                    suspend_schedule,
                    language,
                })
            } else {
                let quotes = match &req {
                    AddQuote::OneByOne { quote } => vec![quote.trim().to_string()],
                    AddQuote::Bulk { quote_bulk } => quote_bulk
                        .lines()
                        .filter(|s| !s.is_empty())
                        .map(|s| s.trim().to_string())
                        .collect(),
                };
                let (cron_input, dedup_duration_minutes, suspend_schedule) =
                    load_cronjob(&user.domain, &user.handle)
                        .await
                        .unwrap_or_else(|error| {
                            tracing::error!(?error, "failed to load schedule");
                            (String::new(), 0, false)
                        });

                match add_quotes(&user.domain, &user.handle, quotes).await {
                    Ok(quotes) => Ok(IndexLoginTemplate {
                        user,
                        quotes,
                        is_bulk_selected: req.is_bulk(),
                        quote_input: String::new(),
                        quote_bulk_input: String::new(),
                        quote_error: None,
                        cron_input,
                        cron_error: None,
                        dedup_duration_minutes,
                        suspend_schedule,
                        language,
                    }),
                    Err(error) => Ok(IndexLoginTemplate {
                        user,
                        quotes: Vec::new(),
                        is_bulk_selected: req.is_bulk(),
                        quote_input: req.as_one_by_one(),
                        quote_bulk_input: req.as_bulk(),
                        quote_error: Some(TemplateError {
                            summary: t(&language, "add-quote-error"),
                            detail: Some(format!("{error:?}")),
                        }),
                        cron_input,
                        cron_error: None,
                        dedup_duration_minutes,
                        suspend_schedule,
                        language,
                    }),
                }
            }
        }
        (
            Some(user),
            PostIndexReq::ConfigureSchedule {
                cron,
                suspend,
                dedup_duration_minutes,
            },
        ) => {
            let suspend = suspend == "on";
            let dedup_duration_minutes = dedup_duration_minutes.parse::<u32>().unwrap_or(0);
            let quotes = load_quotes(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to load quotes");
                    Vec::new()
                });

            if cron.is_empty() {
                return Ok(IndexLoginTemplate {
                    user,
                    quotes,
                    is_bulk_selected: false,
                    quote_input: String::new(),
                    quote_bulk_input: String::new(),
                    quote_error: None,
                    cron_input: String::new(),
                    cron_error: Some(TemplateError {
                        summary: t(&language, "value-cannot-empty"),
                        detail: None,
                    }),
                    dedup_duration_minutes,
                    suspend_schedule: suspend,
                    language,
                });
            }

            match save_cronjob(
                &user.domain,
                &user.handle,
                &user.access_token,
                &user.software,
                &cron,
                dedup_duration_minutes,
                suspend,
            )
            .await
            {
                Ok(()) => Ok(IndexLoginTemplate {
                    user,
                    quotes,
                    is_bulk_selected: false,
                    quote_input: String::new(),
                    quote_bulk_input: String::new(),
                    quote_error: None,
                    cron_input: cron,
                    cron_error: None,
                    dedup_duration_minutes,
                    suspend_schedule: suspend,
                    language,
                }),
                Err(error) => Ok(IndexLoginTemplate {
                    user,
                    quotes,
                    is_bulk_selected: false,
                    quote_input: String::new(),
                    quote_bulk_input: String::new(),
                    quote_error: None,
                    cron_input: cron,
                    cron_error: Some(TemplateError {
                        summary: t(&language, "configure-schedule-error"),
                        detail: Some(format!("{error:?}")),
                    }),
                    dedup_duration_minutes,
                    suspend_schedule: suspend,
                    language,
                }),
            }
        }
        (Some(user), PostIndexReq::DeleteQuote { quote_id }) => {
            let quotes = delete_quote(&user.domain, &user.handle, &quote_id)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to load quotes");
                    Vec::new()
                });
            let (cron_input, dedup_duration_minutes, suspend_schedule) =
                load_cronjob(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load schedule");
                        (String::new(), 0, false)
                    });

            Ok(IndexLoginTemplate {
                user,
                quotes,
                is_bulk_selected: false,
                quote_input: String::new(),
                quote_bulk_input: String::new(),
                quote_error: None,
                cron_input,
                cron_error: None,
                dedup_duration_minutes,
                suspend_schedule,
                language,
            })
        }
        _ => Err(Err(IndexLogoutTemplate {
            language,
            domain: String::new(),
            domain_error: None,
        })),
    }
}
