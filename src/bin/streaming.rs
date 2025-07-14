#[path = "lib/post.rs"]
mod post;

use std::{collections::BTreeMap, sync::LazyLock};

use eyre::WrapErr;
use futures_util::{SinkExt, StreamExt};
use itertools::Itertools;
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

async fn shutdown_signal(stopper: stopper::Stopper) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("signal received, starting graceful shutdown");
    stopper.stop();
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

    let stopper = stopper::Stopper::new();
    tokio::spawn(shutdown_signal(stopper.clone()));

    loop {
        let res = match config.software.as_ref() {
            "mastodon" => {
                stream_mastodon(
                    &config.domain,
                    &config.access_token,
                    &config.replies_configmap_name,
                    &kube_client,
                    &mut rng,
                    &stopper,
                )
                .await
            }
            "misskey" => {
                stream_misskey(
                    &config.domain,
                    &config.access_token,
                    &config.replies_configmap_name,
                    &kube_client,
                    &mut rng,
                    &stopper,
                )
                .await
            }
            software => panic!("unsupported software `{software}`"),
        };
        if let Err(error) = res {
            tracing::error!(?error, "failed to stream. retrying...");
        } else {
            break;
        }
    }
}

async fn get_reply_map_and_dice_feature(
    kube_client: &kube::Client,
    configmap_name: &str,
) -> eyre::Result<(BTreeMap<String, BTreeMap<Ulid, String>>, bool)> {
    let configmap_api = Api::<ConfigMap>::default_namespaced(kube_client.clone());

    let replies_configmap = configmap_api
        .get(configmap_name)
        .await
        .wrap_err("failed to get replies Kubernetes ConfigMap")?;
    let dice_feature = replies_configmap
        .metadata
        .annotations
        .unwrap_or_default()
        .get("fediq.pbzweihander.dev/dice-feature")
        .map(|v| v == "true")
        .unwrap_or_default();
    let reply_map = replies_configmap
        .data
        .unwrap_or_default()
        .get("data")
        .and_then(|v| serde_json::from_str::<BTreeMap<String, BTreeMap<Ulid, String>>>(v).ok())
        .unwrap_or_default();
    Ok((reply_map, dice_feature))
}

async fn stream_mastodon(
    domain: &str,
    access_token: &str,
    configmap_name: &str,
    kube_client: &kube::Client,
    rng: &mut impl rand::Rng,
    stopper: &stopper::Stopper,
) -> eyre::Result<()> {
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Event {
        event: String,
        payload: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Account {
        acct: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Status {
        id: String,
        content: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(tag = "type", rename_all = "camelCase")]
    enum EventInner {
        Mention {
            account: Account,
            status: Status,
        },
        #[serde(other)]
        Unknown,
    }

    let resp = post::HTTP_CLIENT
        .get(format!(
            "wss://{domain}/api/v1/streaming?stream=user:notification"
        ))
        .bearer_auth(access_token)
        .upgrade()
        .send()
        .await
        .wrap_err("failed to request websocket")?;
    let stream = resp
        .into_websocket()
        .await
        .wrap_err("failed to connect websocket")?;
    let mut stream = stopper.stop_stream(stream);
    while let Some(message) = stream
        .next()
        .await
        .transpose()
        .wrap_err("failed to get message")?
    {
        if let reqwest_websocket::Message::Text(payload) = message {
            if let Ok(Event { event, payload }) = serde_json::from_str::<Event>(&payload) {
                if event == "notification" {
                    if let Ok(EventInner::Mention { account, status }) =
                        serde_json::from_str::<EventInner>(&payload)
                    {
                        tracing::info!(?account, ?status, "got mention");
                        let (reply_map, dice_feature) =
                            get_reply_map_and_dice_feature(kube_client, configmap_name)
                                .await
                                .wrap_err("failed to get reply map and dice feature")?;
                        if let Some(reply) = get_reply(&status.content, &reply_map, rng) {
                            tracing::info!(reply, "replying");
                            if let Err(error) = post::post_mastodon(
                                domain,
                                access_token,
                                &format!("@{} {}", account.acct, reply),
                                Some(status.id),
                            )
                            .await
                            {
                                tracing::error!(?error, "failed to post reply to Mastodon");
                            }
                            continue;
                        }
                        if dice_feature {
                            if let Some(dice_result) = get_dice(&status.content, rng) {
                                tracing::info!(dice_result, "replying dice result");
                                if let Err(error) = post::post_mastodon(
                                    domain,
                                    access_token,
                                    &format!("@{} {}", account.acct, dice_result),
                                    Some(status.id),
                                )
                                .await
                                {
                                    tracing::error!(
                                        ?error,
                                        "failed to post dice result to Mastodon"
                                    );
                                }
                                continue;
                            }
                        }
                        tracing::info!("no match");
                    }
                }
            }
        }
    }
    Ok(())
}

async fn stream_misskey(
    domain: &str,
    access_token: &str,
    configmap_name: &str,
    kube_client: &kube::Client,
    rng: &mut impl rand::Rng,
    stopper: &stopper::Stopper,
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
        .wrap_err("failed to request websocket")?;
    let mut stream = resp
        .into_websocket()
        .await
        .wrap_err("failed to connect websocket")?;
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
        .wrap_err("failed to connect channel")?;
    stream.flush().await.wrap_err("failed to flush stream")?;
    let mut stream = stopper.stop_stream(stream);
    while let Some(message) = stream
        .next()
        .await
        .transpose()
        .wrap_err("failed to get message")?
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
                    tracing::info!(?user, text, "got mention");
                    let (reply_map, dice_feature) =
                        get_reply_map_and_dice_feature(kube_client, configmap_name)
                            .await
                            .wrap_err("failed to get reply map and dice feature")?;
                    if let Some(reply) = get_reply(&text, &reply_map, rng) {
                        tracing::info!(reply, "replying");
                        if let Err(error) = post::post_misskey(
                            domain,
                            access_token,
                            &format!("@{}@{} {}", user.username, user.host, reply),
                            Some(note_id.clone()),
                        )
                        .await
                        {
                            tracing::error!(?error, "failed to post to Misskey");
                            continue;
                        }
                    }
                    if dice_feature {
                        if let Some(dice_result) = get_dice(&text, rng) {
                            tracing::info!(dice_result, "replying dice result");
                            if let Err(error) = post::post_misskey(
                                domain,
                                access_token,
                                &format!("@{}@{} {}", user.username, user.host, dice_result),
                                Some(note_id),
                            )
                            .await
                            {
                                tracing::error!(?error, "failed to post dice result to Misskey");
                            }
                            continue;
                        }
                    }
                    tracing::info!("no match");
                }
            }
        }
    }
    Ok(())
}

fn get_reply(
    text: &str,
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

fn get_dice(text: &str, rng: &mut impl rand::Rng) -> Option<String> {
    static DICE_REGEX: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(\d+)d(\d+)").expect("failed to build regex"));
    if let Some(captures) = DICE_REGEX.captures(text) {
        let n = captures.get(1)?.as_str().parse::<usize>().ok()?;
        let m = captures.get(2)?.as_str().parse::<usize>().ok()?;
        if n == 0 || m == 0 {
            return None;
        }

        if n == 1 {
            return Some(rng.random_range(1..=m).to_string());
        }

        let mut list = Vec::new();
        for _i in 0..n {
            let result = rng.random_range(1..=m);
            list.push(result);
        }
        let sum = list.iter().sum::<usize>();

        if n <= 10 && m <= 100 {
            Some(format!(
                "{} = {}",
                list.iter().map(usize::to_string).join(" + "),
                sum
            ))
        } else {
            Some(format!("{sum}"))
        }
    } else {
        None
    }
}
