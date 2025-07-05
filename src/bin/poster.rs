use eyre::Context;
use http::HeaderMap;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    api::{Patch, PatchParams},
    core::ObjectMeta,
    Api,
};
use once_cell::sync::Lazy;
use rand::seq::IteratorRandom;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use ulid::Ulid;

#[derive(Deserialize)]
struct Config {
    domain: String,
    access_token: String,
    software: String,
    quotes_configmap_name: String,
    quote_dedup_configmap_name: String,
    dedup_duration_minutes: u32,
}

static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    let mut headers = HeaderMap::new();
    headers.insert(
        "user-agent",
        concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"))
            .parse()
            .expect("failed to parse header value"),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("failed to build HTTP client")
});

#[derive(Serialize)]
struct PostMastodonReq<'a> {
    status: &'a str,
    visibility: &'a str,
}

async fn post_mastodon(domain: &str, access_token: &str, quote: &str) -> eyre::Result<()> {
    let req = PostMastodonReq {
        status: quote,
        visibility: "unlisted",
    };
    let url = format!("https://{domain}/api/v1/statuses");
    let resp = HTTP_CLIENT
        .post(&url)
        .bearer_auth(access_token)
        .json(&req)
        .send()
        .await
        .with_context(|| format!("failed to request to `{url}`"))?;
    let resp_status = resp.status();
    let resp_text = resp
        .text()
        .await
        .with_context(|| format!("failed to read response from `{url}`"))?;
    if !resp_status.is_success() {
        Err(eyre::eyre!("error response received: `{resp_text}`"))
    } else {
        Ok(())
    }
}

#[derive(Serialize)]
struct PostMisskeyReq<'a> {
    i: &'a str,
    text: &'a str,
    visibility: &'a str,
}

async fn post_misskey(domain: &str, access_token: &str, quote: &str) -> eyre::Result<()> {
    let req = PostMisskeyReq {
        i: access_token,
        text: quote,
        visibility: "home",
    };
    let url = format!("https://{domain}/api/notes/create");
    let resp = HTTP_CLIENT
        .post(&url)
        .json(&req)
        .send()
        .await
        .with_context(|| format!("failed to request to `{url}`"))?;
    let resp_status = resp.status();
    let resp_text = resp
        .text()
        .await
        .with_context(|| format!("failed to read response from `{url}`"))?;
    if !resp_status.is_success() {
        Err(eyre::eyre!("error response received: `{resp_text}`"))
    } else {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let now = OffsetDateTime::now_utc();
    let mut rng = rand::rng();
    let config =
        envy::from_env::<Config>().context("failed to parse config from environment variables")?;

    let kube_client = kube::Client::try_default()
        .await
        .context("failed to initialize Kubernetes client")?;

    let configmap_api = Api::<ConfigMap>::default_namespaced(kube_client);

    let quotes_configmap = configmap_api
        .get_opt(&config.quotes_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get quotes Kubernetes ConfigMap `{}`",
                config.quotes_configmap_name
            )
        })?;
    let Some(quotes_configmap) = quotes_configmap else {
        return Ok(());
    };
    let Some(quotes_configmap_data) = quotes_configmap.data else {
        return Ok(());
    };

    let quote_dedup_configmap = configmap_api
        .get_opt(&config.quote_dedup_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get quote dedup Kubernetes ConfigMap `{}`",
                config.quote_dedup_configmap_name
            )
        })?;
    let mut quote_dedup_configmap_data = quote_dedup_configmap
        .and_then(|cm| cm.data)
        .unwrap_or_default();

    let quotes = quotes_configmap_data
        .into_iter()
        .filter_map(|(key, value)| {
            let id = Ulid::from_string(&key).ok()?;
            let sent_recently = quote_dedup_configmap_data
                .get(&key)
                .and_then(|value| {
                    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                        .ok()
                })
                .map(|timestamp| timestamp > now)
                .unwrap_or(false);
            if sent_recently {
                None
            } else {
                Some((id, value))
            }
        });

    let quote = quotes.choose(&mut rng);
    let Some((quote_id, quote)) = quote else {
        return Ok(());
    };

    match config.software.as_str() {
        "mastodon" => post_mastodon(&config.domain, &config.access_token, &quote)
            .await
            .with_context(|| format!("failed to post to Mastodon `{}`", config.domain))?,
        "misskey" => post_misskey(&config.domain, &config.access_token, &quote)
            .await
            .with_context(|| format!("failed to post to Misskey `{}`", config.domain))?,
        software => {
            return Err(eyre::eyre!("unsupported software `{}`", software));
        }
    }

    let dedup_timestamp = now + Duration::minutes(config.dedup_duration_minutes as i64);
    quote_dedup_configmap_data.insert(
        quote_id.to_string(),
        dedup_timestamp
            .format(&time::format_description::well_known::Rfc3339)
            .context("failed to format OffsetDateTime")?,
    );

    configmap_api
        .patch(
            &config.quote_dedup_configmap_name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(ConfigMap {
                metadata: ObjectMeta {
                    name: Some(config.quote_dedup_configmap_name.clone()),
                    ..Default::default()
                },
                data: Some(quote_dedup_configmap_data),
                ..Default::default()
            }),
        )
        .await
        .with_context(|| {
            format!(
                "failed to patch Kubernetes ConfigMap `{}`",
                config.quote_dedup_configmap_name
            )
        })?;

    Ok(())
}
