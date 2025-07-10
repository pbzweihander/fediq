use std::collections::BTreeMap;

use base64::Engine;
use eyre::Context;
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        batch::v1::{CronJob, CronJobSpec, JobSpec, JobTemplateSpec},
        core::v1::{ConfigMap, Container, EnvVar, PodSpec, PodTemplateSpec, Secret},
    },
    apimachinery::pkg::apis::meta::v1::LabelSelector,
    ByteString,
};
use kube::{
    api::{Patch, PatchParams},
    core::ObjectMeta,
    Api, ResourceExt,
};
use once_cell::sync::OnceCell;
use time::OffsetDateTime;
use ulid::Ulid;

use crate::config::CONFIG;

use super::fediverse::FediverseApp;

const DEDUP_DURATION_MINUTES_ANNOTATION_KEY: &str = "fediq.pbzweihander.dev/dedup-duration-minutes";
const DICE_FEATURE_ANNOTATION_KEY: &str = "fediq.pbzweihander.dev/dice-feature";

async fn client() -> eyre::Result<kube::Client> {
    static CLIENT: OnceCell<kube::Client> = OnceCell::new();
    if let Some(client) = CLIENT.get() {
        Ok(client.clone())
    } else {
        let client = kube::Client::try_default()
            .await
            .context("failed to initialize Kubernetes client")?;
        let _ = CLIENT.set(client.clone());
        Ok(client)
    }
}

fn fediverse_app_secret_name(domain: &str) -> String {
    format!("fediq-fediverse-app-{domain}")
        .to_ascii_lowercase()
        .replace('_', "-")
}

fn quotes_configmap_name(domain: &str, handle: &str) -> String {
    format!("fediq-quotes-{domain}-{handle}")
        .to_ascii_lowercase()
        .replace('_', "-")
}

fn replies_configmap_name(domain: &str, handle: &str) -> String {
    format!("fediq-replies-{domain}-{handle}")
        .to_ascii_lowercase()
        .replace('_', "-")
}

fn quote_dedup_configmap_name(domain: &str, handle: &str) -> String {
    format!("fediq-quote-dedup-{domain}-{handle}")
        .to_ascii_lowercase()
        .replace('_', "-")
}

fn poster_cronjob_name(domain: &str, handle: &str) -> String {
    format!("fediq-poster-{domain}-{handle}")
        .to_ascii_lowercase()
        .replace('_', "-")
}

fn streaming_deployment_name(domain: &str, handle: &str) -> String {
    format!("fediq-streaming-{domain}-{handle}")
        .to_ascii_lowercase()
        .replace('_', "-")
}

pub async fn load_fediverse_app(domain: &str) -> eyre::Result<Option<FediverseApp>> {
    let client = client().await?;
    let secret_api = Api::<Secret>::default_namespaced(client);
    let secret = secret_api
        .get_opt(&fediverse_app_secret_name(domain))
        .await
        .with_context(|| {
            format!("failed to get fediverse app Kubernetes Secret for domain `{domain}`")
        })?;
    let Some(secret) = secret else {
        return Ok(None);
    };

    let data = secret.data.unwrap_or_default();

    let Some(client_id) = data.get("client_id") else {
        return Ok(None);
    };
    let Some(client_secret) = data.get("client_secret") else {
        return Ok(None);
    };

    let client_id = base64::engine::general_purpose::STANDARD
        .decode(&client_id.0)
        .context("failed to decode client id secret data")?;
    let client_secret = base64::engine::general_purpose::STANDARD
        .decode(&client_secret.0)
        .context("failed to decode client secret secret data")?;

    let client_id =
        String::from_utf8(client_id).context("failed to decode client id as UTF-8 string")?;
    let client_secret = String::from_utf8(client_secret)
        .context("failed to decode client secret as UTF-8 string")?;

    Ok(Some(FediverseApp {
        client_id,
        client_secret,
    }))
}

pub async fn save_fediverse_app(domain: &str, app: &FediverseApp) -> eyre::Result<()> {
    let client = client().await?;
    let secret_api = Api::<Secret>::default_namespaced(client);

    let name = fediverse_app_secret_name(domain);

    let client_id = base64::engine::general_purpose::STANDARD.encode(app.client_id.as_bytes());
    let client_secret =
        base64::engine::general_purpose::STANDARD.encode(app.client_secret.as_bytes());

    let mut data = BTreeMap::new();
    data.insert("client_id".to_string(), ByteString(client_id.into_bytes()));
    data.insert(
        "client_secret".to_string(),
        ByteString(client_secret.into_bytes()),
    );

    secret_api
        .patch(
            &name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(Secret {
                metadata: ObjectMeta {
                    name: Some(name.clone()),
                    ..Default::default()
                },
                data: Some(data),
                ..Default::default()
            }),
        )
        .await
        .with_context(|| format!("failed to patch Kubernetes Secret `{name}`"))?;

    Ok(())
}

fn quote_map_to_template_map(
    quotes_map: &BTreeMap<String, String>,
    quote_dedup_map: &BTreeMap<String, String>,
) -> BTreeMap<Ulid, (String, bool)> {
    let now = OffsetDateTime::now_utc();

    let quotes = quotes_map
        .iter()
        .filter_map(|(key, value)| {
            let id = Ulid::from_string(key).ok()?;
            let sent_recently = quote_dedup_map
                .get(key)
                .and_then(|value| {
                    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                        .ok()
                })
                .map(|timestamp| timestamp > now)
                .unwrap_or(false);
            Some((id, (value.clone(), sent_recently)))
        })
        .collect::<BTreeMap<_, _>>();

    quotes
}

pub async fn load_quotes(
    domain: &str,
    handle: &str,
) -> eyre::Result<BTreeMap<Ulid, (String, bool)>> {
    let client = client().await?;
    let configmap_api = Api::<ConfigMap>::default_namespaced(client);

    let quotes_configmap_name = quotes_configmap_name(domain, handle);
    let quotes_configmap = configmap_api
        .get_opt(&quotes_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get quotes Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let Some(quotes_configmap) = quotes_configmap else {
        return Ok(BTreeMap::new());
    };
    let Some(quotes_configmap_data) = quotes_configmap.data else {
        return Ok(BTreeMap::new());
    };

    let quote_dedup_configmap_name = quote_dedup_configmap_name(domain, handle);
    let quote_dedup_configmap = configmap_api.get_opt(&quote_dedup_configmap_name).await
        .with_context(|| {
            format!(
                "failed to get quote dedup Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let quote_dedup_configmap_data = quote_dedup_configmap
        .and_then(|cm| cm.data)
        .unwrap_or_default();

    Ok(quote_map_to_template_map(
        &quotes_configmap_data,
        &quote_dedup_configmap_data,
    ))
}

pub async fn add_quotes(
    domain: &str,
    handle: &str,
    quotes: Vec<String>,
) -> eyre::Result<BTreeMap<Ulid, (String, bool)>> {
    let client = client().await?;
    let configmap_api = Api::<ConfigMap>::default_namespaced(client);

    let quotes_configmap_name = quotes_configmap_name(domain, handle);
    let quotes_configmap = configmap_api
        .get_opt(&quotes_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let mut quotes_configmap_data = quotes_configmap.and_then(|cm| cm.data).unwrap_or_default();

    let mut quote_id = Ulid::new();
    for quote in quotes {
        quote_id = quote_id.increment().unwrap_or_default();
        quotes_configmap_data.insert(quote_id.to_string(), quote);
    }

    let quote_dedup_configmap_name = quote_dedup_configmap_name(domain, handle);
    let quote_dedup_configmap = configmap_api.get_opt(&quote_dedup_configmap_name).await
        .with_context(|| {
            format!(
                "failed to get quote dedup Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let quote_dedup_configmap_data = quote_dedup_configmap
        .and_then(|cm| cm.data)
        .unwrap_or_default();

    let quotes = quote_map_to_template_map(&quotes_configmap_data, &quote_dedup_configmap_data);

    configmap_api
        .patch(
            &quotes_configmap_name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(ConfigMap {
                metadata: ObjectMeta {
                    name: Some(quotes_configmap_name.clone()),
                    ..Default::default()
                },
                data: Some(quotes_configmap_data),
                ..Default::default()
            }),
        )
        .await
        .with_context(|| {
            format!("failed to patch Kubernetes ConfigMap `{quotes_configmap_name}`")
        })?;

    Ok(quotes)
}

pub async fn delete_quote(
    domain: &str,
    handle: &str,
    id: Ulid,
) -> eyre::Result<BTreeMap<Ulid, (String, bool)>> {
    let client = client().await?;
    let configmap_api = Api::<ConfigMap>::default_namespaced(client);

    let quotes_configmap_name = quotes_configmap_name(domain, handle);
    let quotes_configmap = configmap_api
        .get_opt(&quotes_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let mut quotes_configmap_data = quotes_configmap.and_then(|cm| cm.data).unwrap_or_default();

    quotes_configmap_data.remove(&id.to_string());

    let quote_dedup_configmap_name = quote_dedup_configmap_name(domain, handle);
    let quote_dedup_configmap = configmap_api.get_opt(&quote_dedup_configmap_name).await
        .with_context(|| {
            format!(
                "failed to get quote dedup Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let quote_dedup_configmap_data = quote_dedup_configmap
        .and_then(|cm| cm.data)
        .unwrap_or_default();

    let quotes = quote_map_to_template_map(&quotes_configmap_data, &quote_dedup_configmap_data);

    configmap_api
        .patch(
            &quotes_configmap_name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(ConfigMap {
                metadata: ObjectMeta {
                    name: Some(quotes_configmap_name.clone()),
                    ..Default::default()
                },
                data: Some(quotes_configmap_data),
                ..Default::default()
            }),
        )
        .await
        .with_context(|| {
            format!("failed to patch Kubernetes ConfigMap `{quotes_configmap_name}`")
        })?;

    Ok(quotes)
}

pub async fn load_cronjob(domain: &str, handle: &str) -> eyre::Result<(String, u32, bool)> {
    let client = client().await?;
    let cronjob_api = Api::<CronJob>::default_namespaced(client);

    let poster_cronjob_name = poster_cronjob_name(domain, handle);
    let poster_cronjob = cronjob_api
        .get_opt(&poster_cronjob_name)
        .await
        .with_context(|| {
            format!("failed to get quotes Kubernetes Cronjob for domain `{domain}` and handle `{handle}`")
        })?;
    let Some(poster_cronjob) = poster_cronjob else {
        return Ok((String::new(), 0, false));
    };
    let Some(poster_cronjob_spec) = &poster_cronjob.spec else {
        return Ok((String::new(), 0, false));
    };

    let dedup_duration_minutes = poster_cronjob
        .annotations()
        .get(DEDUP_DURATION_MINUTES_ANNOTATION_KEY)
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);

    Ok((
        poster_cronjob_spec.schedule.clone(),
        dedup_duration_minutes,
        poster_cronjob_spec.suspend.unwrap_or(false),
    ))
}

pub async fn save_cronjob(
    domain: &str,
    handle: &str,
    access_token: &str,
    software: &str,
    cron: &str,
    dedup_duration_minutes: u32,
    suspend: bool,
) -> eyre::Result<()> {
    let client = client().await?;
    let cronjob_api = Api::<CronJob>::default_namespaced(client);

    let mut poster_cronjob_annotations = BTreeMap::<String, String>::new();
    poster_cronjob_annotations.insert(
        DEDUP_DURATION_MINUTES_ANNOTATION_KEY.to_string(),
        dedup_duration_minutes.to_string(),
    );

    let poster_cronjob_name = poster_cronjob_name(domain, handle);
    let poster_cronjob = CronJob {
        metadata: ObjectMeta {
            name: Some(poster_cronjob_name.clone()),
            annotations: Some(poster_cronjob_annotations),
            ..Default::default()
        },
        spec: Some(CronJobSpec {
            schedule: cron.to_string(),
            suspend: Some(suspend),
            starting_deadline_seconds: Some(360),
            job_template: JobTemplateSpec {
                metadata: None,
                spec: Some(JobSpec {
                    template: PodTemplateSpec {
                        metadata: None,
                        spec: Some(PodSpec {
                            service_account_name: Some(CONFIG.poster_serviceaccount_name.clone()),
                            restart_policy: Some("Never".to_string()),
                            containers: vec![Container {
                                name: "fediq-poster".to_string(),
                                image: Some(CONFIG.poster_container_image.clone()),
                                command: Some(vec!["fediq-poster".to_string()]),
                                env: Some(vec![
                                    EnvVar {
                                        name: "DOMAIN".to_string(),
                                        value: Some(domain.to_string()),
                                        value_from: None,
                                    },
                                    EnvVar {
                                        name: "ACCESS_TOKEN".to_string(),
                                        value: Some(access_token.to_string()),
                                        value_from: None,
                                    },
                                    EnvVar {
                                        name: "SOFTWARE".to_string(),
                                        value: Some(software.to_string()),
                                        value_from: None,
                                    },
                                    EnvVar {
                                        name: "QUOTES_CONFIGMAP_NAME".to_string(),
                                        value: Some(quotes_configmap_name(domain, handle)),
                                        value_from: None,
                                    },
                                    EnvVar {
                                        name: "QUOTE_DEDUP_CONFIGMAP_NAME".to_string(),
                                        value: Some(quote_dedup_configmap_name(domain, handle)),
                                        value_from: None,
                                    },
                                    EnvVar {
                                        name: "DEDUP_DURATION_MINUTES".to_string(),
                                        value: Some(dedup_duration_minutes.to_string()),
                                        value_from: None,
                                    },
                                ]),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }),
                    },
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    };

    cronjob_api
        .patch(
            &poster_cronjob_name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(poster_cronjob),
        )
        .await
        .with_context(|| format!("failed to patch Kubernetes CronJob `{poster_cronjob_name}`"))?;

    Ok(())
}

fn deserialize_reply_map(
    data: BTreeMap<String, String>,
) -> BTreeMap<String, BTreeMap<Ulid, String>> {
    data.get("data")
        .and_then(|v| serde_json::from_str::<BTreeMap<String, BTreeMap<Ulid, String>>>(v).ok())
        .unwrap_or_default()
}

fn serialize_reply_map(
    reply_map: &BTreeMap<String, BTreeMap<Ulid, String>>,
) -> BTreeMap<String, String> {
    let data = serde_json::to_string(reply_map).unwrap();
    let mut output = BTreeMap::new();
    output.insert("data".to_string(), data);
    output
}

pub async fn load_replies(
    domain: &str,
    handle: &str,
) -> eyre::Result<BTreeMap<String, BTreeMap<Ulid, String>>> {
    let client = client().await?;
    let configmap_api = Api::<ConfigMap>::default_namespaced(client);

    let replies_configmap_name = replies_configmap_name(domain, handle);
    let replies_configmap = configmap_api
        .get_opt(&replies_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get replies Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let replies_configmap_data = replies_configmap.and_then(|cm| cm.data).unwrap_or_default();

    let reply_map = deserialize_reply_map(replies_configmap_data);

    Ok(reply_map)
}

pub async fn add_replies(
    domain: &str,
    handle: &str,
    keyword: String,
    replies: Vec<String>,
) -> eyre::Result<BTreeMap<String, BTreeMap<Ulid, String>>> {
    let client = client().await?;
    let configmap_api = Api::<ConfigMap>::default_namespaced(client);

    let replies_configmap_name = replies_configmap_name(domain, handle);
    let replies_configmap = configmap_api
        .get_opt(&replies_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get replies Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let annotations = replies_configmap
        .as_ref()
        .and_then(|cm| cm.metadata.annotations.clone())
        .unwrap_or_else(|| {
            let mut annotation = BTreeMap::new();
            annotation.insert(DICE_FEATURE_ANNOTATION_KEY.to_string(), "false".to_string());
            annotation
        });
    let replies_configmap_data = replies_configmap.and_then(|cm| cm.data).unwrap_or_default();

    let mut reply_map = deserialize_reply_map(replies_configmap_data);

    reply_map
        .entry(keyword)
        .or_default()
        .extend(replies.into_iter().map({
            let mut reply_id = Ulid::new();
            move |reply| {
                reply_id = reply_id.increment().unwrap_or_default();
                (reply_id, reply)
            }
        }));

    let data_to_save = serialize_reply_map(&reply_map);

    configmap_api
        .patch(
            &replies_configmap_name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(ConfigMap {
                metadata: ObjectMeta {
                    name: Some(replies_configmap_name.clone()),
                    annotations: Some(annotations),
                    ..Default::default()
                },
                data: Some(data_to_save),
                ..Default::default()
            }),
        )
        .await
        .with_context(|| {
            format!("failed to patch Kubernetes ConfigMap `{replies_configmap_name}`")
        })?;

    Ok(reply_map)
}

pub async fn delete_reply(
    domain: &str,
    handle: &str,
    keyword: String,
    id: Ulid,
) -> eyre::Result<BTreeMap<String, BTreeMap<Ulid, String>>> {
    let client = client().await?;
    let configmap_api = Api::<ConfigMap>::default_namespaced(client);

    let replies_configmap_name = replies_configmap_name(domain, handle);
    let replies_configmap = configmap_api
        .get_opt(&replies_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let annotations = replies_configmap
        .as_ref()
        .and_then(|cm| cm.metadata.annotations.clone())
        .unwrap_or_else(|| {
            let mut annotation = BTreeMap::new();
            annotation.insert(DICE_FEATURE_ANNOTATION_KEY.to_string(), "false".to_string());
            annotation
        });
    let replies_configmap_data = replies_configmap.and_then(|cm| cm.data).unwrap_or_default();

    let mut reply_map = deserialize_reply_map(replies_configmap_data);

    reply_map.entry(keyword).and_modify(|m| {
        m.remove(&id);
    });

    let data_to_save = serialize_reply_map(&reply_map);

    configmap_api
        .patch(
            &replies_configmap_name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(ConfigMap {
                metadata: ObjectMeta {
                    name: Some(replies_configmap_name.clone()),
                    annotations: Some(annotations),
                    ..Default::default()
                },
                data: Some(data_to_save),
                ..Default::default()
            }),
        )
        .await
        .with_context(|| {
            format!("failed to patch Kubernetes ConfigMap `{replies_configmap_name}`")
        })?;

    Ok(reply_map)
}

pub async fn delete_reply_all(
    domain: &str,
    handle: &str,
    keyword: String,
) -> eyre::Result<BTreeMap<String, BTreeMap<Ulid, String>>> {
    let client = client().await?;
    let configmap_api = Api::<ConfigMap>::default_namespaced(client);

    let replies_configmap_name = replies_configmap_name(domain, handle);
    let replies_configmap = configmap_api
        .get_opt(&replies_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let annotations = replies_configmap
        .as_ref()
        .and_then(|cm| cm.metadata.annotations.clone())
        .unwrap_or_else(|| {
            let mut annotation = BTreeMap::new();
            annotation.insert(DICE_FEATURE_ANNOTATION_KEY.to_string(), "false".to_string());
            annotation
        });
    let replies_configmap_data = replies_configmap.and_then(|cm| cm.data).unwrap_or_default();

    let mut reply_map = deserialize_reply_map(replies_configmap_data);

    reply_map.remove(&keyword);

    let data_to_save = serialize_reply_map(&reply_map);

    configmap_api
        .patch(
            &replies_configmap_name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(ConfigMap {
                metadata: ObjectMeta {
                    name: Some(replies_configmap_name.clone()),
                    annotations: Some(annotations),
                    ..Default::default()
                },
                data: Some(data_to_save),
                ..Default::default()
            }),
        )
        .await
        .with_context(|| {
            format!("failed to patch Kubernetes ConfigMap `{replies_configmap_name}`")
        })?;

    Ok(reply_map)
}

pub async fn get_reply_enabled(domain: &str, handle: &str) -> eyre::Result<bool> {
    let client = client().await?;
    let deployment_api = Api::<Deployment>::default_namespaced(client);

    let deployment_name = streaming_deployment_name(domain, handle);
    let deployment = deployment_api
        .get_opt(&deployment_name)
        .await
        .with_context(|| format!("failed to get Kubernetes Deployment `{deployment_name}`"))?;

    Ok(deployment.is_some())
}

pub async fn enable_reply(
    domain: &str,
    handle: &str,
    access_token: &str,
    software: &str,
) -> eyre::Result<()> {
    let client = client().await?;
    let deployment_api = Api::<Deployment>::default_namespaced(client);

    let deployment_name = streaming_deployment_name(domain, handle);
    let mut pod_labels = BTreeMap::new();
    pod_labels.insert(
        "app.kubernetes.io/name".to_string(),
        "fediq-streaming".to_string(),
    );
    pod_labels.insert(
        "app.kubernetes.io/instance".to_string(),
        deployment_name.to_string(),
    );
    let deployment = Deployment {
        metadata: ObjectMeta {
            name: Some(deployment_name.clone()),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(1),
            selector: LabelSelector {
                match_labels: Some(pod_labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(pod_labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    service_account_name: Some(CONFIG.streaming_serviceaccount_name.clone()),
                    containers: vec![Container {
                        name: "streaming".to_string(),
                        image: Some(CONFIG.streaming_container_image.clone()),
                        command: Some(vec!["fediq-streaming".to_string()]),
                        env: Some(vec![
                            EnvVar {
                                name: "DOMAIN".to_string(),
                                value: Some(domain.to_string()),
                                value_from: None,
                            },
                            EnvVar {
                                name: "ACCESS_TOKEN".to_string(),
                                value: Some(access_token.to_string()),
                                value_from: None,
                            },
                            EnvVar {
                                name: "SOFTWARE".to_string(),
                                value: Some(software.to_string()),
                                value_from: None,
                            },
                            EnvVar {
                                name: "REPLIES_CONFIGMAP_NAME".to_string(),
                                value: Some(replies_configmap_name(domain, handle)),
                                value_from: None,
                            },
                        ]),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    };

    deployment_api
        .patch(
            &deployment_name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(deployment),
        )
        .await
        .with_context(|| format!("failed to patch Kubernetes Deoloyment `{deployment_name}`"))?;

    Ok(())
}

pub async fn disable_reply(domain: &str, handle: &str) -> eyre::Result<()> {
    let client = client().await?;
    let deployment_api = Api::<Deployment>::default_namespaced(client);

    let deployment_name = streaming_deployment_name(domain, handle);
    if let Err(error) = deployment_api
        .delete(&deployment_name, &Default::default())
        .await
    {
        if let kube::Error::Api(kube::error::ErrorResponse { reason, .. }) = &error {
            if reason == "NotFound" {
                return Ok(());
            }
        }
        return Err(eyre::Report::new(error).wrap_err(format!(
            "failed to delete Kubernetes Deployment `{deployment_name}`"
        )));
    }

    Ok(())
}

pub async fn get_dice_feature_enabled(domain: &str, handle: &str) -> eyre::Result<bool> {
    let client = client().await?;
    let configmap_api = Api::<ConfigMap>::default_namespaced(client);

    let replies_configmap_name = replies_configmap_name(domain, handle);
    let replies_configmap = configmap_api
        .get_opt(&replies_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    Ok(replies_configmap
        .and_then(|cm| cm.metadata.annotations)
        .map(|an| an.get(DICE_FEATURE_ANNOTATION_KEY).map(String::as_str) == Some("true"))
        .unwrap_or_default())
}

pub async fn enable_dice_feature(domain: &str, handle: &str) -> eyre::Result<()> {
    let client = client().await?;
    let configmap_api = Api::<ConfigMap>::default_namespaced(client);

    let replies_configmap_name = replies_configmap_name(domain, handle);
    let replies_configmap = configmap_api
        .get_opt(&replies_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let mut annotations = replies_configmap
        .as_ref()
        .and_then(|cm| cm.metadata.annotations.clone())
        .unwrap_or_default();
    let configmap_data = replies_configmap.and_then(|cm| cm.data).unwrap_or_default();
    annotations.insert(DICE_FEATURE_ANNOTATION_KEY.to_string(), "true".to_string());

    configmap_api
        .patch(
            &replies_configmap_name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(ConfigMap {
                metadata: ObjectMeta {
                    name: Some(replies_configmap_name.clone()),
                    annotations: Some(annotations),
                    ..Default::default()
                },
                data: Some(configmap_data),
                ..Default::default()
            }),
        )
        .await
        .with_context(|| {
            format!("failed to patch Kubernetes ConfigMap `{replies_configmap_name}`")
        })?;

    Ok(())
}

pub async fn disable_dice_feature(domain: &str, handle: &str) -> eyre::Result<()> {
    let client = client().await?;
    let configmap_api = Api::<ConfigMap>::default_namespaced(client);

    let replies_configmap_name = replies_configmap_name(domain, handle);
    let replies_configmap = configmap_api
        .get_opt(&replies_configmap_name)
        .await
        .with_context(|| {
            format!(
                "failed to get Kubernetes ConfigMap for domain `{domain}` and handle `{handle}`"
            )
        })?;
    let mut annotations = replies_configmap
        .as_ref()
        .and_then(|cm| cm.metadata.annotations.clone())
        .unwrap_or_default();
    let configmap_data = replies_configmap.and_then(|cm| cm.data).unwrap_or_default();
    annotations.insert(DICE_FEATURE_ANNOTATION_KEY.to_string(), "false".to_string());

    configmap_api
        .patch(
            &replies_configmap_name,
            &PatchParams::apply("fediq.pbzweihander.dev"),
            &Patch::Apply(ConfigMap {
                metadata: ObjectMeta {
                    name: Some(replies_configmap_name.clone()),
                    annotations: Some(annotations),
                    ..Default::default()
                },
                data: Some(configmap_data),
                ..Default::default()
            }),
        )
        .await
        .with_context(|| {
            format!("failed to patch Kubernetes ConfigMap `{replies_configmap_name}`")
        })?;

    Ok(())
}
