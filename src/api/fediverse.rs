use std::{str::FromStr, time::Duration};

use eyre::{OptionExt, WrapErr};
use http::HeaderMap;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    api::kube::{load_fediverse_app, save_fediverse_app},
    config::CONFIG,
    handler::auth::FediverseUser,
};

static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    let mut headers = HeaderMap::new();
    headers.insert(
        "user-agent",
        "fediq.pbzweihander.dev"
            .parse()
            .expect("failed to parse header value"),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(5))
        .build()
        .expect("failed to build HTTP client")
});

pub struct FediverseApp {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Deserialize)]
struct NodeInfoSoftware {
    name: String,
}

#[derive(Deserialize)]
struct NodeInfo {
    software: NodeInfoSoftware,
}

#[derive(Deserialize)]
struct WellKnownNodeInfoLink {
    rel: String,
    href: String,
}

#[derive(Deserialize)]
struct WellKnownNodeInfo {
    links: Vec<WellKnownNodeInfoLink>,
}

pub async fn get_software_name(domain: &str) -> eyre::Result<String> {
    let url = format!("https://{domain}/.well-known/nodeinfo");
    let resp_text = HTTP_CLIENT
        .get(&url)
        .send()
        .await
        .wrap_err_with(|| format!("failed to request to `{url}`"))?
        .text()
        .await
        .wrap_err_with(|| format!("failed to read response from `{url}`"))?;
    let resp = serde_json::from_str::<WellKnownNodeInfo>(&resp_text)
        .wrap_err_with(|| format!("failed to parse response `{resp_text}`"))?;
    let nodeinfo_href = resp
        .links
        .into_iter()
        .find(|link| link.rel == "http://nodeinfo.diaspora.software/ns/schema/2.0")
        .ok_or_else(|| eyre::eyre!("nodeinfo link not found from response `{}`", resp_text))?
        .href;

    let resp_text = HTTP_CLIENT
        .get(&nodeinfo_href)
        .send()
        .await
        .wrap_err_with(|| format!("failed to request to `{nodeinfo_href}`"))?
        .text()
        .await
        .wrap_err_with(|| format!("failed to read response from `{nodeinfo_href}`"))?;
    let resp = serde_json::from_str::<NodeInfo>(&resp_text)
        .wrap_err_with(|| format!("failed to parse response `{resp_text}`"))?;

    Ok(resp.software.name)
}

#[derive(Serialize)]
struct MastodonCreateAppReq<'a> {
    client_name: &'a str,
    redirect_uris: &'a Url,
    scopes: &'a str,
    website: Url,
}

#[derive(Deserialize)]
struct MastodonCreateAppResp {
    client_id: String,
    client_secret: String,
}

async fn get_auth_redirect_url_mastodon(domain: &str) -> eyre::Result<Url> {
    let redirect_url = CONFIG
        .public_url
        .join(&format!("./auth/callback/mastodon/{domain}"))
        .wrap_err("failed to generate redirect URL")?;
    let app = if let Some(app) = load_fediverse_app(domain)
        .await
        .wrap_err("failed to load fediverse app from Kubernetes")?
    {
        app
    } else {
        let req = MastodonCreateAppReq {
            client_name: if cfg!(debug_assertions) {
                "fediq-debug.pbzweihander.dev"
            } else {
                "fediq.pbzweihander.dev"
            },
            redirect_uris: &redirect_url,
            scopes: "read:accounts write:statuses read:notifications",
            website: CONFIG.public_url.clone(),
        };
        let url = format!("https://{domain}/api/v1/apps");
        let resp_text = HTTP_CLIENT
            .post(&url)
            .json(&req)
            .send()
            .await
            .wrap_err_with(|| format!("failed to request to `{url}`"))?
            .text()
            .await
            .wrap_err_with(|| format!("failed to read response from `{url}`"))?;
        let resp = serde_json::from_str::<MastodonCreateAppResp>(&resp_text)
            .wrap_err_with(|| format!("failed to parse response `{resp_text}`"))?;

        let app = FediverseApp {
            client_id: resp.client_id,
            client_secret: resp.client_secret,
        };
        save_fediverse_app(domain, &app)
            .await
            .wrap_err("failed to save fediverse app to Kubernetes")?;
        app
    };

    let client_id = app.client_id;

    let url = Url::from_str(&format!("https://{domain}/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri={redirect_url}&scope=read:accounts+write:statuses+read:notifications"))
        .wrap_err("failed to generate URL")?;
    Ok(url)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MisskeyCreateAppReq<'a> {
    name: &'a str,
    description: String,
    permission: &'a [&'a str],
    callback_url: Url,
}

#[derive(Deserialize)]
struct MisskeyCreateAppResp {
    id: String,
    secret: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MisskeyGenerateSessionReq<'a> {
    app_secret: &'a str,
}

#[derive(Deserialize)]
struct MisskeyGenerateSessionResp {
    url: Url,
}

async fn get_auth_redirect_url_misskey(domain: &str) -> eyre::Result<Url> {
    let app = if let Some(app) = load_fediverse_app(domain)
        .await
        .wrap_err("failed to load fediverse app from Kubernetes")?
    {
        app
    } else {
        let redirect_url = CONFIG
            .public_url
            .join(&format!("./auth/callback/misskey/{domain}"))
            .wrap_err("failed to generate redirect URL")?;
        let req = MisskeyCreateAppReq {
            name: if cfg!(debug_assertions) {
                "fediq-debug.pbzweihander.dev"
            } else {
                "fediq.pbzweihander.dev"
            },
            description: CONFIG.public_url.to_string(),
            permission: &["read:account", "write:notes", "read:notifications"],
            callback_url: redirect_url,
        };
        let url = format!("https://{domain}/api/app/create");
        let resp_text = HTTP_CLIENT
            .post(&url)
            .json(&req)
            .send()
            .await
            .wrap_err_with(|| format!("failed to request to `{url}`"))?
            .text()
            .await
            .wrap_err_with(|| format!("failed to read response from `{url}`"))?;
        let resp = serde_json::from_str::<MisskeyCreateAppResp>(&resp_text)
            .wrap_err_with(|| format!("failed to parse response `{resp_text}`"))?;

        let app = FediverseApp {
            client_id: resp.id,
            client_secret: resp.secret,
        };
        save_fediverse_app(domain, &app)
            .await
            .wrap_err("failed to save fediverse app to Kubernetes")?;
        app
    };

    let url = format!("https://{domain}/api/auth/session/generate");
    let req = MisskeyGenerateSessionReq {
        app_secret: &app.client_secret,
    };
    let resp_text = HTTP_CLIENT
        .post(&url)
        .json(&req)
        .send()
        .await
        .wrap_err_with(|| format!("failed to request to `{url}`"))?
        .text()
        .await
        .wrap_err_with(|| format!("failed to read response from `{url}`"))?;
    let resp = serde_json::from_str::<MisskeyGenerateSessionResp>(&resp_text)
        .wrap_err_with(|| format!("failed to parse response `{resp_text}`"))?;
    Ok(resp.url)
}

pub async fn get_auth_redirect_url(domain: &str) -> eyre::Result<Url> {
    let software_name = get_software_name(domain).await?;

    match software_name.as_str() {
        "mastodon" => get_auth_redirect_url_mastodon(domain).await,
        "misskey" | "cherrypick" | "firefish" => get_auth_redirect_url_misskey(domain).await,
        name => Err(eyre::eyre!("unsupported software `{}`", name)),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MisskeySessionUserKeyReq<'a> {
    app_secret: &'a str,
    token: &'a str,
}

#[derive(Serialize)]
struct MastodonObtainTokenReq<'a> {
    grant_type: &'a str,
    code: &'a str,
    client_id: &'a str,
    client_secret: &'a str,
    redirect_uri: &'a Url,
    scope: &'a str,
}

#[derive(Deserialize)]
struct MastodonObtainTokenResp {
    access_token: String,
}

#[derive(Deserialize)]
struct MastodonAccount {
    username: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    avatar: Option<Url>,
}

pub async fn login_mastodon(domain: &str, code: &str) -> eyre::Result<FediverseUser> {
    let app = load_fediverse_app(domain)
        .await
        .wrap_err("failed to load fediverse app from Kubernetes")?
        .ok_or_eyre("fediverse app not found in Kubernetes")?;

    let redirect_url = CONFIG
        .public_url
        .join(&format!("./auth/callback/mastodon/{domain}"))
        .wrap_err("failed to generate redirect URL")?;
    let req = MastodonObtainTokenReq {
        grant_type: "authorization_code",
        code,
        client_id: &app.client_id,
        client_secret: &app.client_secret,
        redirect_uri: &redirect_url,
        scope: "read:accounts read:statuses read:notifications write:statuses",
    };
    let url = format!("https://{domain}/oauth/token");
    let resp_text = HTTP_CLIENT
        .post(&url)
        .json(&req)
        .send()
        .await
        .wrap_err_with(|| format!("failed to request to `{url}`"))?
        .text()
        .await
        .wrap_err_with(|| format!("failed to read response from `{url}`"))?;
    let resp = serde_json::from_str::<MastodonObtainTokenResp>(&resp_text)
        .wrap_err_with(|| format!("failed to parse response `{resp_text}`"))?;

    let access_token = resp.access_token;

    let url = format!("https://{domain}/api/v1/accounts/verify_credentials");
    let resp_text = HTTP_CLIENT
        .get(&url)
        .bearer_auth(&access_token)
        .send()
        .await
        .wrap_err_with(|| format!("failed to request to `{url}`"))?
        .text()
        .await
        .wrap_err_with(|| format!("failed to read response from `{url}`"))?;
    let resp = serde_json::from_str::<MastodonAccount>(&resp_text)
        .wrap_err_with(|| format!("failed to parse response `{resp_text}`"))?;

    Ok(FediverseUser::new(
        domain.to_string(),
        resp.username,
        resp.display_name,
        resp.avatar,
        access_token,
        "mastodon".to_string(),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MisskeyUser {
    username: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    avatar_url: Option<Url>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MisskeySessionUserKeyResp {
    access_token: String,
    user: MisskeyUser,
}

pub async fn login_misskey(domain: &str, token: &str) -> eyre::Result<FediverseUser> {
    let app = load_fediverse_app(domain)
        .await
        .wrap_err("failed to load fediverse app from Kubernetes")?
        .ok_or_eyre("fediverse app not found in Kubernetes")?;

    let req = MisskeySessionUserKeyReq {
        app_secret: &app.client_secret,
        token,
    };
    let url = format!("https://{domain}/api/auth/session/userkey");
    let resp_text = HTTP_CLIENT
        .post(&url)
        .json(&req)
        .send()
        .await
        .wrap_err_with(|| format!("failed to request to `{url}`"))?
        .text()
        .await
        .wrap_err_with(|| format!("failed to read response from `{url}`"))?;
    let resp = serde_json::from_str::<MisskeySessionUserKeyResp>(&resp_text)
        .wrap_err_with(|| format!("failed to parse response `{resp_text}`"))?;

    Ok(FediverseUser::new(
        domain.to_string(),
        resp.user.username,
        resp.user.name,
        resp.user.avatar_url,
        resp.access_token,
        "misskey".to_string(),
    ))
}
