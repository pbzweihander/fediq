#[path = "lib/post.rs"]
mod post;

use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    api::{Patch, PatchParams},
    core::ObjectMeta,
    Api,
};
use rand::seq::IteratorRandom;
use serde::Deserialize;
use time::{Duration, OffsetDateTime};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
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

#[tokio::main]
async fn main() {
    color_eyre::install().expect("failed to install color-eyre");

    tracing_subscriber::registry()
        .with(tracing_error::ErrorLayer::default())
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let now = OffsetDateTime::now_utc();
    let mut rng = rand::rng();
    let config =
        envy::from_env::<Config>().expect("failed to parse config from environment variables");

    let kube_client = kube::Client::try_default()
        .await
        .expect("failed to initialize Kubernetes client");

    let configmap_api = Api::<ConfigMap>::default_namespaced(kube_client);

    let quotes_configmap_data = configmap_api
        .get(&config.quotes_configmap_name)
        .await
        .expect("failed to get quotes Kubernetes ConfigMap")
        .data
        .unwrap_or_default();

    let quote_dedup_configmap = configmap_api
        .get_opt(&config.quote_dedup_configmap_name)
        .await
        .expect("failed to get quote dedup Kubernetes ConfigMap");
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
        return;
    };

    match config.software.as_str() {
        "mastodon" => post::post_mastodon(&config.domain, &config.access_token, &quote, None)
            .await
            .expect("failed to post to Mastodon"),
        "misskey" => post::post_misskey(&config.domain, &config.access_token, &quote, None)
            .await
            .expect("failed to post to Misskey"),
        software => {
            panic!("unsupported software `{software}`");
        }
    }

    let dedup_timestamp = now + Duration::minutes(config.dedup_duration_minutes as i64);
    quote_dedup_configmap_data.insert(
        quote_id.to_string(),
        dedup_timestamp
            .format(&time::format_description::well_known::Rfc3339)
            .expect("failed to format OffsetDateTime"),
    );

    configmap_api
        .patch(
            &config.quote_dedup_configmap_name,
            &PatchParams::apply("fediq.pbzweihander.dev").force(),
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
        .expect("failed to patch Kubernetes ConfigMap");
}
