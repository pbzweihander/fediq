#[path = "lib/post.rs"]
mod post;

use std::collections::BTreeMap;

use eyre::Context;
use futures_util::{SinkExt, StreamExt};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::Api;
use rand::seq::IteratorRandom;
use reqwest_websocket::RequestBuilderExt;
use serde::{Deserialize, Serialize};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use ulid::Ulid;

#[derive(Deserialize)]
struct Config {
    domain: String,
    access_token: String,
    software: String,
    replies_configmap_name: String,
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

    let mut rng = rand::rng();
    let config =
        envy::from_env::<Config>().expect("failed to parse config from environment variables");

    let kube_client = kube::Client::try_default()
        .await
        .expect("failed to initialize Kubernetes client");
    let configmap_api = Api::<ConfigMap>::default_namespaced(kube_client);

    let replies_configmap_data = configmap_api
        .get(&config.replies_configmap_name)
        .await
        .expect("failed to get replies Kubernetes ConfigMap")
        .data
        .unwrap_or_default();
    let reply_map = replies_configmap_data
        .get("data")
        .and_then(|v| serde_json::from_str::<BTreeMap<String, BTreeMap<Ulid, String>>>(v).ok())
        .expect("no reply data found");
    if reply_map.is_empty() {
        panic!("reply data empty");
    }

    match config.software.as_ref() {
        "mastodon" => stream_mastodon(config.domain, config.access_token, reply_map, &mut rng)
            .await
            .expect("failed to stream Mastodon"),
        "misskey" => stream_misskey(config.domain, config.access_token, reply_map, &mut rng)
            .await
            .expect("failed to stream Misskey"),
        software => panic!("unsupported software `{software}`"),
    }
}

async fn stream_mastodon(
    domain: String,
    access_token: String,
    reply_map: BTreeMap<String, BTreeMap<Ulid, String>>,
    rng: &mut impl rand::Rng,
) -> eyre::Result<()> {
    let resp = post::HTTP_CLIENT
        .get(format!(
            "wss://{domain}/api/v1/streaming?stream=user:notification"
        ))
        .bearer_auth(access_token)
        .upgrade()
        .send()
        .await
        .context("failed to request websocket")?;
    let mut stream = resp
        .into_websocket()
        .await
        .context("failed to connect websocket")?;
    while let Some(message) = stream
        .next()
        .await
        .transpose()
        .context("failed to get message")?
    {
        println!("{message:#?}");
    }
    Ok(())
}

async fn stream_misskey(
    domain: String,
    access_token: String,
    reply_map: BTreeMap<String, BTreeMap<Ulid, String>>,
    rng: &mut impl rand::Rng,
) -> eyre::Result<()> {
    #[derive(Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct User {
        username: String,
        host: String,
    }

    #[derive(Debug, Deserialize, Serialize)]
    #[serde(tag = "type", content = "body", rename_all = "camelCase")]
    enum ChannelMessage {
        Mention {
            id: String,
            user: User,
            text: String,
        },
        #[serde(other)]
        Unknown,
    }

    #[derive(Debug, Deserialize, Serialize)]
    #[serde(tag = "type", content = "body", rename_all = "camelCase")]
    enum Message {
        Connect {
            channel: String,
            id: String,
        },
        Channel {
            id: String,
            #[serde(flatten)]
            body: ChannelMessage,
        },
        #[serde(other)]
        Unknown,
    }

    let resp = post::HTTP_CLIENT
        .get(format!("wss://{domain}/streaming?i={access_token}"))
        .upgrade()
        .send()
        .await
        .context("failed to request websocket")?;
    let mut stream = resp
        .into_websocket()
        .await
        .context("failed to connect websocket")?;
    let channel_id = Ulid::new();
    stream
        .send(reqwest_websocket::Message::Text(
            serde_json::to_string(&Message::Connect {
                channel: "main".to_string(),
                id: channel_id.to_string(),
            })
            .unwrap(),
        ))
        .await
        .context("failed to connect channel")?;
    stream.flush().await.context("failed to flush stream")?;
    while let Some(message) = stream
        .next()
        .await
        .transpose()
        .context("failed to get message")?
    {
        if let reqwest_websocket::Message::Text(payload) = message {
            if let Ok(Message::Channel {
                id: rx_channel_id,
                body:
                    ChannelMessage::Mention {
                        id: note_id,
                        user,
                        text,
                    },
            }) = serde_json::from_str::<Message>(&payload)
            {
                if rx_channel_id == channel_id.to_string() {
                    tracing::info!(text, "got mention");
                    if let Some(reply) = get_reply(text, &reply_map, rng) {
                        tracing::info!(reply, "replying");
                        if let Err(error) = post::post_misskey(
                            &domain,
                            &access_token,
                            &format!("@{}@{} {}", user.username, user.host, reply),
                            Some(note_id),
                        )
                        .await
                        {
                            tracing::error!(?error, "failed to post to Misskey");
                        }
                    } else {
                        tracing::info!("no match");
                    }
                }
            }
        }
    }
    Ok(())
}

fn get_reply(
    text: String,
    reply_map: &BTreeMap<String, BTreeMap<Ulid, String>>,
    rng: &mut impl rand::Rng,
) -> Option<String> {
    for (keyword, replies) in reply_map.iter().rev() {
        if text.contains(keyword) {
            return replies.values().choose(rng).cloned();
        }
    }
    None
}
