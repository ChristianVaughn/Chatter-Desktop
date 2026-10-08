//! The slice of Chatter's HTTP API the test client needs: accounts (with the
//! mandatory TOTP step), rooms, channels and ICE servers.

use anyhow::{bail, Context, Result};
use chatter_media::IceServersResponse;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use totp_rs::{Algorithm, Secret, TOTP};

/// Test accounts this client created, kept out of the repo in `.testclient/`.
#[derive(Default, Serialize, Deserialize)]
pub struct UserStore {
    /// server origin -> username -> account
    servers: BTreeMap<String, BTreeMap<String, StoredUser>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredUser {
    pub password: String,
    pub totp_secret: String,
}

impl UserStore {
    fn path() -> PathBuf {
        PathBuf::from(".testclient").join("users.json")
    }

    pub fn load() -> Result<Self> {
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(".testclient")?;
        std::fs::write(Self::path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn get(&self, server: &str, username: &str) -> Option<&StoredUser> {
        self.servers.get(server)?.get(username)
    }

    pub fn put(&mut self, server: &str, username: &str, user: StoredUser) {
        self.servers
            .entry(server.to_string())
            .or_default()
            .insert(username.to_string(), user);
    }
}

/// Current code for a base32 TOTP secret, as the server checks it
/// (SHA1, 6 digits, 30 s step).
pub fn totp_now(secret_b32: &str) -> Result<String> {
    let bytes = Secret::Encoded(secret_b32.to_string())
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("bad TOTP secret: {e:?}"))?;
    let totp = TOTP::new_unchecked(Algorithm::SHA1, 6, 1, 30, bytes);
    Ok(totp.generate_current()?)
}

pub struct Session {
    pub server: String,
    pub user_id: String,
    pub access_token: String,
    http: reqwest::Client,
}

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("chatter-testclient/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

async fn check(res: reqwest::Response) -> Result<Value> {
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("{status}: {body}");
    }
    Ok(body)
}

/// Create an account, completing the TOTP enrolment the server requires.
pub async fn register(server: &str, username: &str, password: &str) -> Result<StoredUser> {
    let http = http();
    let pending = check(
        http.post(format!("{server}/_matrix/client/r0/register"))
            .json(&json!({ "username": username, "password": password, "password_confirm": password }))
            .send()
            .await?,
    )
    .await
    .context("register")?;
    let user_id = pending["user_id"].as_str().context("no user_id")?;
    let secret = pending["totp_secret"]
        .as_str()
        .context("no totp_secret")?
        .to_string();

    check(
        http.post(format!("{server}/api/totp/verify"))
            .json(&json!({ "user_id": user_id, "code": totp_now(&secret)? }))
            .send()
            .await?,
    )
    .await
    .context("verify TOTP")?;

    Ok(StoredUser {
        password: password.to_string(),
        totp_secret: secret,
    })
}

pub async fn login(server: &str, username: &str, user: &StoredUser) -> Result<Session> {
    let http = http();
    let body = check(
        http.post(format!("{server}/_matrix/client/r0/login"))
            .json(&json!({ "username": username, "password": user.password, "totp_code": totp_now(&user.totp_secret)? }))
            .send()
            .await?,
    )
    .await
    .context("login")?;
    if body["requires_totp"].as_bool() == Some(true) {
        bail!("server still asks for TOTP");
    }
    Ok(Session {
        server: server.to_string(),
        user_id: body["user_id"].as_str().context("no user_id")?.to_string(),
        access_token: body["access_token"]
            .as_str()
            .context("no access_token")?
            .to_string(),
        http,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub channel_id: String,
    pub name: String,
    pub channel_type: String,
    #[serde(default)]
    pub voice_bitrate: Option<f64>,
}

impl Session {
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.bearer_auth(&self.access_token)
    }

    pub async fn create_room(&self, name: &str) -> Result<String> {
        let body = check(
            self.auth(
                self.http
                    .post(format!("{}/_matrix/client/r0/createRoom", self.server)),
            )
            .json(&json!({ "name": name, "topic": "Native voice spike" }))
            .send()
            .await?,
        )
        .await
        .context("createRoom")?;
        Ok(body["room_id"].as_str().context("no room_id")?.to_string())
    }

    pub async fn join_room(&self, room_id: &str) -> Result<()> {
        let path = format!(
            "{}/_matrix/client/r0/rooms/{}/join",
            self.server,
            urlencode(room_id)
        );
        check(
            self.auth(self.http.post(path))
                .json(&json!({}))
                .send()
                .await?,
        )
        .await
        .context("join room")?;
        Ok(())
    }

    pub async fn channels(&self, room_id: &str) -> Result<Vec<Channel>> {
        let path = format!("{}/api/rooms/{}/channels", self.server, urlencode(room_id));
        let body = check(self.auth(self.http.get(path)).send().await?)
            .await
            .context("list channels")?;
        Ok(serde_json::from_value(body["channels"].clone())?)
    }

    pub async fn ice_servers(&self) -> Result<IceServersResponse> {
        let body = check(
            self.auth(self.http.get(format!("{}/api/ice-servers", self.server)))
                .send()
                .await?,
        )
        .await
        .context("ice servers")?;
        Ok(serde_json::from_value(body)?)
    }

    pub fn ws_url(&self) -> Result<String> {
        let mut url = url::Url::parse(&self.server)?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme).ok();
        url.set_path("/ws");
        Ok(url.to_string())
    }
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
