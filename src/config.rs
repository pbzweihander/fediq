use eyre::Context;
use once_cell::sync::Lazy;
use serde::Deserialize;
use url::Url;

pub static CONFIG: Lazy<Config> = Lazy::new(|| Config::try_from_env().unwrap());

fn default_listen_addr() -> String {
    "0.0.0.0:3000".to_string()
}

fn deserialize_jwt_secret<'de, D>(
    d: D,
) -> Result<(jsonwebtoken::EncodingKey, jsonwebtoken::DecodingKey), D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    Ok((
        jsonwebtoken::EncodingKey::from_secret(s.as_bytes()),
        jsonwebtoken::DecodingKey::from_secret(s.as_bytes()),
    ))
}

#[derive(Deserialize)]
pub struct Config {
    pub public_url: Url,

    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,

    #[serde(deserialize_with = "deserialize_jwt_secret")]
    pub jwt_secret: (jsonwebtoken::EncodingKey, jsonwebtoken::DecodingKey),

    pub poster_container_image: String,
    pub poster_serviceaccount_name: String,

    pub streaming_container_image: String,
    pub streaming_serviceaccount_name: String,
}

impl Config {
    pub fn try_from_env() -> eyre::Result<Self> {
        envy::from_env().context("failed to read config from environment variables")
    }
}
