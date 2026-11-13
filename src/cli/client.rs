//! HTTP client used by the CLI.
//!
//! Normal commands never open SQLite directly; they ask the daemon through
//! the local API. A stopped daemon therefore fails with a clear message.

use std::fs;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::config::{AppPaths, Credentials};

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:3000";

pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    pub fn from_env(paths: &AppPaths) -> Result<Self> {
        let base = fs::read_to_string(paths.daemon_endpoint_file())
            .map(|value| value.trim().to_string())
            .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
        let credentials = Credentials::load(&paths.credentials_file())
            .context("Mindmount daemon is not running.\nStart it with: mindmount daemon start")?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("cannot create the HTTP client")?;
        Ok(Self {
            http,
            base,
            token: credentials.mcp_token,
        })
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.get_query(path, Vec::new()).await
    }

    pub async fn get_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: Vec<(String, String)>,
    ) -> Result<T> {
        let request = self.http.get(format!("{}{path}", self.base)).query(&query);
        self.decode(self.send(request).await?).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let request = self.http.post(format!("{}{path}", self.base)).json(body);
        self.decode(self.send(request).await?).await
    }

    pub async fn put<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let request = self.http.put(format!("{}{path}", self.base)).json(body);
        self.decode(self.send(request).await?).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let request = self.http.delete(format!("{}{path}", self.base));
        self.decode(self.send(request).await?).await
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Vec<u8>> {
        let response = request
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(connection_error)?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .context("cannot read the API response")?;
        if !status.is_success() {
            let message = serde_json::from_slice::<ApiErrorBody>(&body)
                .map(|error| error.error)
                .unwrap_or_else(|_| String::from_utf8_lossy(&body).into_owned());
            bail!("{message}");
        }
        Ok(body.to_vec())
    }

    async fn decode<T: DeserializeOwned>(&self, body: Vec<u8>) -> Result<T> {
        serde_json::from_slice(&body).context("cannot parse the API response")
    }
}

#[derive(serde::Deserialize)]
struct ApiErrorBody {
    error: String,
}

fn connection_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_connect() {
        anyhow::anyhow!("Mindmount daemon is not running.\nStart it with: mindmount daemon start")
    } else if error.is_timeout() {
        anyhow::anyhow!("the Mindmount daemon did not respond in time")
    } else {
        anyhow::Error::new(error).context("cannot reach the Mindmount daemon")
    }
}
