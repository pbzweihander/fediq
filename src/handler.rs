pub mod auth;
mod extract;
mod templates;

use std::collections::BTreeMap;

use askama::Template;
use axum::{
    response::{Html, Redirect},
    routing, Form, Router,
};
use axum_extra::{headers, TypedHeader};
use serde::Deserialize;
use ulid::Ulid;

use crate::{
    api::{
        fediverse::get_auth_redirect_url,
        kube::{
            add_quotes, add_replies, delete_quote, delete_reply, disable_reply, enable_reply,
            get_reply_enabled, load_cronjob, load_quotes, load_replies, save_cronjob,
        },
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

fn fmt_user(user: &Result<FediverseUser, ()>) -> String {
    if let Ok(user) = user {
        user.to_string()
    } else {
        "(none)".to_string()
    }
}

#[tracing::instrument(skip_all, fields(user = fmt_user(&user)))]
async fn get_index(Language(language): Language, user: Result<FediverseUser, ()>) -> Html<String> {
    if let Ok(user) = user {
        let quotes = load_quotes(&user.domain, &user.handle)
            .await
            .unwrap_or_else(|error| {
                tracing::error!(?error, "failed to load quotes");
                BTreeMap::new()
            });
        let (cron_input, dedup_duration_minutes, suspend_schedule) =
            load_cronjob(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to load schedule");
                    (String::new(), 0, false)
                });
        let reply_map = load_replies(&user.domain, &user.handle)
            .await
            .unwrap_or_else(|error| {
                tracing::error!(?error, "failed to load replies");
                BTreeMap::new()
            });
        let enable_reply = get_reply_enabled(&user.domain, &user.handle)
            .await
            .unwrap_or_else(|error| {
                tracing::error!(?error, "failed to get reply enabled");
                false
            });

        Html(
            IndexLoginTemplate {
                language,
                user,
                quote_mode_selected: true,
                quotes,
                is_quote_bulk_selected: false,
                quote_input: String::new(),
                quote_bulk_input: String::new(),
                quote_error: None,
                cron_input,
                cron_error: None,
                dedup_duration_minutes,
                suspend_schedule,
                enable_reply,
                is_reply_bulk_selected: false,
                reply_map,
                reply_keyword_input: String::new(),
                reply_input: String::new(),
                reply_bulk_input: String::new(),
                reply_error: None,
            }
            .render()
            .unwrap(),
        )
    } else {
        Html(
            IndexLogoutTemplate {
                language,
                domain: String::new(),
                domain_error: None,
            }
            .render()
            .unwrap(),
        )
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
#[serde(rename_all = "snake_case", tag = "add_reply_mode")]
enum AddReply {
    OneByOne {
        #[serde(default)]
        keyword: String,
        #[serde(default)]
        reply: String,
    },
    Bulk {
        #[serde(default)]
        keyword: String,
        #[serde(default)]
        reply_bulk: String,
    },
}

impl AddReply {
    fn is_empty(&self) -> bool {
        match self {
            Self::OneByOne { keyword, reply } => keyword.is_empty() || reply.is_empty(),
            Self::Bulk {
                keyword,
                reply_bulk,
            } => keyword.is_empty() || reply_bulk.is_empty(),
        }
    }

    fn is_bulk(&self) -> bool {
        matches!(
            self,
            Self::Bulk {
                keyword: _,
                reply_bulk: _
            }
        )
    }

    fn keyword(&self) -> String {
        match self {
            Self::OneByOne { keyword, reply: _ } => keyword.clone(),
            Self::Bulk {
                keyword,
                reply_bulk: _,
            } => keyword.clone(),
        }
    }

    fn as_one_by_one(&self) -> String {
        match self {
            Self::OneByOne { keyword: _, reply } => reply.clone(),
            Self::Bulk {
                keyword: _,
                reply_bulk: _,
            } => String::new(),
        }
    }

    fn as_bulk(&self) -> String {
        match self {
            Self::OneByOne {
                keyword: _,
                reply: _,
            } => String::new(),
            Self::Bulk {
                keyword: _,
                reply_bulk,
            } => reply_bulk.clone(),
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
    AddReply(AddReply),
    DeleteReply {
        keyword: String,
        reply_id: Ulid,
    },
    ConfigureReply {
        #[serde(default)]
        enable: String,
    },
}

#[tracing::instrument(skip_all, fields(user = fmt_user(&user)))]
async fn post_index(
    user: Result<FediverseUser, ()>,
    Language(language): Language,
    Form(req): Form<PostIndexReq>,
) -> Result<Html<String>, Redirect> {
    match (user, req) {
        (_, PostIndexReq::Login { domain }) => {
            if domain.is_empty() {
                return Ok(Html(
                    IndexLogoutTemplate {
                        domain,
                        domain_error: Some(TemplateError {
                            summary: t(&language, "value-cannot-empty"),
                            detail: None,
                        }),
                        language,
                    }
                    .render()
                    .unwrap(),
                ));
            }

            match get_auth_redirect_url(&domain).await {
                Ok(redirect_url) => Err(Redirect::to(redirect_url.as_str())),
                Err(error) => {
                    tracing::warn!(?error, "failed to get auth redirect URL");
                    Ok(Html(
                        IndexLogoutTemplate {
                            domain,
                            domain_error: Some(TemplateError {
                                summary: t(&language, "login-error"),
                                detail: Some(format!("{error:?}")),
                            }),
                            language,
                        }
                        .render()
                        .unwrap(),
                    ))
                }
            }
        }
        (Err(()), _) => Ok(Html(
            IndexLogoutTemplate {
                language,
                domain: String::new(),
                domain_error: None,
            }
            .render()
            .unwrap(),
        )),
        (Ok(user), PostIndexReq::AddQuote(req)) => {
            if req.is_empty() {
                let quotes = load_quotes(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load quotes");
                        BTreeMap::new()
                    });
                let (cron_input, dedup_duration_minutes, suspend_schedule) =
                    load_cronjob(&user.domain, &user.handle)
                        .await
                        .unwrap_or_else(|error| {
                            tracing::error!(?error, "failed to load schedule");
                            (String::new(), 0, false)
                        });
                let reply_map = load_replies(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load replies");
                        BTreeMap::new()
                    });
                let enable_reply = get_reply_enabled(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to get reply enabled");
                        false
                    });

                Ok(Html(
                    IndexLoginTemplate {
                        user,
                        quote_mode_selected: true,
                        quotes,
                        is_quote_bulk_selected: req.is_bulk(),
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
                        enable_reply,
                        is_reply_bulk_selected: false,
                        reply_map,
                        reply_keyword_input: String::new(),
                        reply_input: String::new(),
                        reply_bulk_input: String::new(),
                        reply_error: None,
                        language,
                    }
                    .render()
                    .unwrap(),
                ))
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
                let reply_map = load_replies(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load replies");
                        BTreeMap::new()
                    });
                let enable_reply = get_reply_enabled(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to get reply enabled");
                        false
                    });

                match add_quotes(&user.domain, &user.handle, quotes).await {
                    Ok(quotes) => Ok(Html(
                        IndexLoginTemplate {
                            user,
                            quote_mode_selected: true,
                            quotes,
                            is_quote_bulk_selected: req.is_bulk(),
                            quote_input: String::new(),
                            quote_bulk_input: String::new(),
                            quote_error: None,
                            cron_input,
                            cron_error: None,
                            dedup_duration_minutes,
                            suspend_schedule,
                            enable_reply,
                            is_reply_bulk_selected: false,
                            reply_map,
                            reply_keyword_input: String::new(),
                            reply_input: String::new(),
                            reply_bulk_input: String::new(),
                            reply_error: None,
                            language,
                        }
                        .render()
                        .unwrap(),
                    )),
                    Err(error) => {
                        tracing::warn!(?error, "failed to add quotes");
                        let quotes = load_quotes(&user.domain, &user.handle)
                            .await
                            .unwrap_or_else(|error| {
                                tracing::error!(?error, "failed to load quotes");
                                BTreeMap::new()
                            });
                        Ok(Html(
                            IndexLoginTemplate {
                                user,
                                quote_mode_selected: true,
                                quotes,
                                is_quote_bulk_selected: req.is_bulk(),
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
                                enable_reply,
                                is_reply_bulk_selected: false,
                                reply_map,
                                reply_keyword_input: String::new(),
                                reply_input: String::new(),
                                reply_bulk_input: String::new(),
                                reply_error: None,
                                language,
                            }
                            .render()
                            .unwrap(),
                        ))
                    }
                }
            }
        }
        (
            Ok(user),
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
                    BTreeMap::new()
                });
            let reply_map = load_replies(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to load replies");
                    BTreeMap::new()
                });
            let enable_reply = get_reply_enabled(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to get reply enabled");
                    false
                });

            if cron.is_empty() {
                return Ok(Html(
                    IndexLoginTemplate {
                        user,
                        quote_mode_selected: true,
                        quotes,
                        is_quote_bulk_selected: false,
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
                        enable_reply,
                        is_reply_bulk_selected: false,
                        reply_map,
                        reply_keyword_input: String::new(),
                        reply_input: String::new(),
                        reply_bulk_input: String::new(),
                        reply_error: None,
                        language,
                    }
                    .render()
                    .unwrap(),
                ));
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
                Ok(()) => Ok(Html(
                    IndexLoginTemplate {
                        user,
                        quote_mode_selected: true,
                        quotes,
                        is_quote_bulk_selected: false,
                        quote_input: String::new(),
                        quote_bulk_input: String::new(),
                        quote_error: None,
                        cron_input: cron,
                        cron_error: None,
                        enable_reply,
                        dedup_duration_minutes,
                        suspend_schedule: suspend,
                        is_reply_bulk_selected: false,
                        reply_map,
                        reply_keyword_input: String::new(),
                        reply_input: String::new(),
                        reply_bulk_input: String::new(),
                        reply_error: None,
                        language,
                    }
                    .render()
                    .unwrap(),
                )),
                Err(error) => {
                    tracing::warn!(?error, "failed to save cronjob");
                    Ok(Html(
                        IndexLoginTemplate {
                            user,
                            quote_mode_selected: true,
                            quotes,
                            is_quote_bulk_selected: false,
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
                            enable_reply,
                            is_reply_bulk_selected: false,
                            reply_map,
                            reply_keyword_input: String::new(),
                            reply_input: String::new(),
                            reply_bulk_input: String::new(),
                            reply_error: None,
                            language,
                        }
                        .render()
                        .unwrap(),
                    ))
                }
            }
        }
        (Ok(user), PostIndexReq::DeleteQuote { quote_id }) => {
            let quotes = match delete_quote(&user.domain, &user.handle, quote_id).await {
                Ok(quotes) => quotes,
                Err(error) => {
                    tracing::error!(?error, "failed to delete quotes");
                    load_quotes(&user.domain, &user.handle)
                        .await
                        .unwrap_or_else(|error| {
                            tracing::error!(?error, "failed to load quotes");
                            BTreeMap::new()
                        })
                }
            };
            let (cron_input, dedup_duration_minutes, suspend_schedule) =
                load_cronjob(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load schedule");
                        (String::new(), 0, false)
                    });
            let reply_map = load_replies(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to load replies");
                    BTreeMap::new()
                });
            let enable_reply = get_reply_enabled(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to get reply enabled");
                    false
                });

            Ok(Html(
                IndexLoginTemplate {
                    user,
                    quote_mode_selected: false,
                    quotes,
                    is_quote_bulk_selected: false,
                    quote_input: String::new(),
                    quote_bulk_input: String::new(),
                    quote_error: None,
                    cron_input,
                    cron_error: None,
                    dedup_duration_minutes,
                    suspend_schedule,
                    enable_reply,
                    is_reply_bulk_selected: false,
                    reply_map,
                    reply_keyword_input: String::new(),
                    reply_input: String::new(),
                    reply_bulk_input: String::new(),
                    reply_error: None,
                    language,
                }
                .render()
                .unwrap(),
            ))
        }
        (Ok(user), PostIndexReq::AddReply(req)) => {
            if req.is_empty() {
                let quotes = load_quotes(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load quotes");
                        BTreeMap::new()
                    });
                let (cron_input, dedup_duration_minutes, suspend_schedule) =
                    load_cronjob(&user.domain, &user.handle)
                        .await
                        .unwrap_or_else(|error| {
                            tracing::error!(?error, "failed to load schedule");
                            (String::new(), 0, false)
                        });
                let reply_map = load_replies(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load replies");
                        BTreeMap::new()
                    });
                let enable_reply = get_reply_enabled(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to get reply enabled");
                        false
                    });

                Ok(Html(
                    IndexLoginTemplate {
                        user,
                        quote_mode_selected: false,
                        quotes,
                        is_quote_bulk_selected: false,
                        quote_input: String::new(),
                        quote_bulk_input: String::new(),
                        quote_error: None,
                        cron_input,
                        cron_error: None,
                        dedup_duration_minutes,
                        suspend_schedule,
                        enable_reply,
                        is_reply_bulk_selected: req.is_bulk(),
                        reply_map,
                        reply_keyword_input: req.keyword(),
                        reply_input: req.as_one_by_one(),
                        reply_bulk_input: req.as_bulk(),
                        reply_error: Some(TemplateError {
                            summary: t(&language, "value-cannot-empty"),
                            detail: None,
                        }),
                        language,
                    }
                    .render()
                    .unwrap(),
                ))
            } else {
                let (keyword, replies) = match &req {
                    AddReply::OneByOne { keyword, reply } => {
                        (keyword.trim().to_string(), vec![reply.trim().to_string()])
                    }
                    AddReply::Bulk {
                        keyword,
                        reply_bulk,
                    } => (
                        keyword.trim().to_string(),
                        reply_bulk
                            .lines()
                            .filter(|s| !s.is_empty())
                            .map(|s| s.trim().to_string())
                            .collect(),
                    ),
                };
                let quotes = load_quotes(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load quotes");
                        BTreeMap::new()
                    });
                let (cron_input, dedup_duration_minutes, suspend_schedule) =
                    load_cronjob(&user.domain, &user.handle)
                        .await
                        .unwrap_or_else(|error| {
                            tracing::error!(?error, "failed to load schedule");
                            (String::new(), 0, false)
                        });
                let enable_reply = get_reply_enabled(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to get reply enabled");
                        false
                    });

                match add_replies(&user.domain, &user.handle, keyword, replies).await {
                    Ok(reply_map) => Ok(Html(
                        IndexLoginTemplate {
                            user,
                            quote_mode_selected: false,
                            quotes,
                            is_quote_bulk_selected: false,
                            quote_input: String::new(),
                            quote_bulk_input: String::new(),
                            quote_error: None,
                            cron_input,
                            cron_error: None,
                            dedup_duration_minutes,
                            suspend_schedule,
                            enable_reply,
                            is_reply_bulk_selected: req.is_bulk(),
                            reply_map,
                            reply_keyword_input: req.keyword(),
                            reply_input: String::new(),
                            reply_bulk_input: String::new(),
                            reply_error: None,
                            language,
                        }
                        .render()
                        .unwrap(),
                    )),
                    Err(error) => {
                        tracing::warn!(?error, "failed to add replies");
                        let reply_map = load_replies(&user.domain, &user.handle)
                            .await
                            .unwrap_or_else(|error| {
                                tracing::error!(?error, "failed to load replies");
                                BTreeMap::new()
                            });
                        Ok(Html(
                            IndexLoginTemplate {
                                user,
                                quote_mode_selected: false,
                                quotes,
                                is_quote_bulk_selected: false,
                                quote_input: String::new(),
                                quote_bulk_input: String::new(),
                                quote_error: None,
                                cron_input,
                                cron_error: None,
                                dedup_duration_minutes,
                                suspend_schedule,
                                enable_reply,
                                is_reply_bulk_selected: req.is_bulk(),
                                reply_map,
                                reply_keyword_input: req.keyword(),
                                reply_input: req.as_one_by_one(),
                                reply_bulk_input: req.as_bulk(),
                                reply_error: Some(TemplateError {
                                    summary: t(&language, "add-reply-error"),
                                    detail: Some(format!("{error:?}")),
                                }),
                                language,
                            }
                            .render()
                            .unwrap(),
                        ))
                    }
                }
            }
        }
        (Ok(user), PostIndexReq::DeleteReply { keyword, reply_id }) => {
            let reply_map = match delete_reply(&user.domain, &user.handle, keyword, reply_id).await
            {
                Ok(replies) => replies,
                Err(error) => {
                    tracing::error!(?error, "failed to delete reply");
                    load_replies(&user.domain, &user.handle)
                        .await
                        .unwrap_or_else(|error| {
                            tracing::error!(?error, "failed to load replies");
                            BTreeMap::new()
                        })
                }
            };
            let quotes = load_quotes(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to load quotes");
                    BTreeMap::new()
                });
            let (cron_input, dedup_duration_minutes, suspend_schedule) =
                load_cronjob(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load schedule");
                        (String::new(), 0, false)
                    });
            let enable_reply = get_reply_enabled(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to get reply enabled");
                    false
                });

            Ok(Html(
                IndexLoginTemplate {
                    user,
                    quote_mode_selected: false,
                    quotes,
                    is_quote_bulk_selected: false,
                    quote_input: String::new(),
                    quote_bulk_input: String::new(),
                    quote_error: None,
                    cron_input,
                    cron_error: None,
                    dedup_duration_minutes,
                    suspend_schedule,
                    enable_reply,
                    is_reply_bulk_selected: false,
                    reply_map,
                    reply_keyword_input: String::new(),
                    reply_input: String::new(),
                    reply_bulk_input: String::new(),
                    reply_error: None,
                    language,
                }
                .render()
                .unwrap(),
            ))
        }
        (Ok(user), PostIndexReq::ConfigureReply { enable }) => {
            if enable == "on" {
                if let Err(error) = enable_reply(
                    &user.domain,
                    &user.handle,
                    &user.access_token,
                    &user.software,
                )
                .await
                {
                    tracing::error!(?error, "failed to enable reply");
                }
            } else if let Err(error) = disable_reply(&user.domain, &user.handle).await {
                tracing::error!(?error, "failed to disable reply");
            }
            let quotes = load_quotes(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to load quotes");
                    BTreeMap::new()
                });
            let (cron_input, dedup_duration_minutes, suspend_schedule) =
                load_cronjob(&user.domain, &user.handle)
                    .await
                    .unwrap_or_else(|error| {
                        tracing::error!(?error, "failed to load schedule");
                        (String::new(), 0, false)
                    });
            let reply_map = load_replies(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to load replies");
                    BTreeMap::new()
                });
            let enable_reply = get_reply_enabled(&user.domain, &user.handle)
                .await
                .unwrap_or_else(|error| {
                    tracing::error!(?error, "failed to get reply enabled");
                    false
                });

            Ok(Html(
                IndexLoginTemplate {
                    language,
                    user,
                    quote_mode_selected: false,
                    quotes,
                    is_quote_bulk_selected: false,
                    quote_input: String::new(),
                    quote_bulk_input: String::new(),
                    quote_error: None,
                    cron_input,
                    cron_error: None,
                    dedup_duration_minutes,
                    suspend_schedule,
                    enable_reply,
                    is_reply_bulk_selected: false,
                    reply_map,
                    reply_keyword_input: String::new(),
                    reply_input: String::new(),
                    reply_bulk_input: String::new(),
                    reply_error: None,
                }
                .render()
                .unwrap(),
            ))
        }
    }
}
