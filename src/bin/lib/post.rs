use eyre::Context;
use http::HeaderMap;
use once_cell::sync::Lazy;
use serde::Serialize;

pub static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    let mut headers = HeaderMap::new();
    headers.insert(
        "user-agent",
        "fediq.pbzweihander.dev"
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

pub async fn post_mastodon(domain: &str, access_token: &str, quote: &str) -> eyre::Result<()> {
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

pub async fn post_misskey(
    domain: &str,
    access_token: &str,
    text: &str,
    reply_id: Option<String>,
) -> eyre::Result<()> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Req<'a> {
        i: &'a str,
        text: &'a str,
        visibility: &'a str,
        reply_id: Option<String>,
    }

    let req = Req {
        i: access_token,
        text,
        visibility: "home",
        reply_id,
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
