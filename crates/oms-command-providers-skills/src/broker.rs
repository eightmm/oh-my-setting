use crate::common::*;
use crate::https::{HttpsResponse, HttpsTransport};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) fn subscription_env(command: &mut Command) {
    for name in [
        concat!("OPENAI_API_", "KEY"),
        concat!("CODEX_API_", "KEY"),
        concat!("CODEX_ACCESS_", "TOKEN"),
    ] {
        command.env_remove(name);
    }
}
struct Login {
    bearer: Vec<u8>,
    account: String,
    at: Instant,
}
impl Drop for Login {
    fn drop(&mut self) {
        self.bearer.fill(0);
    }
}
pub(crate) struct Broker {
    child: Child,
    input: ChildStdin,
    rows: mpsc::Receiver<Value>,
    ident: u64,
    cached: Option<Login>,
}
impl Broker {
    pub fn start() -> Result<Self> {
        let mut command = Command::new("codex");
        command
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        subscription_env(&mut command);
        let mut child = command.spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| error("missing broker input"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| error("missing broker output"))?;
        let (sender, rows) = mpsc::sync_channel(128);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut line = Vec::new();
                match reader
                    .by_ref()
                    .take(1024 * 1024 + 1)
                    .read_until(b'\n', &mut line)
                {
                    Ok(n) if n > 0 && n <= 1024 * 1024 => (),
                    _ => break,
                }
                let row = crate::common::json(&line);
                line.fill(0);
                match row {
                    Ok(row) if row.get("id").is_some() => {
                        if sender.try_send(row).is_err() {
                            break;
                        }
                    }
                    Ok(_) => (),
                    Err(_) => break,
                }
            }
        });
        let mut result = Self {
            child,
            input,
            rows,
            ident: 0,
            cached: None,
        };
        result.call(
            "initialize",
            json!({"clientInfo":{"name":"oms_harness","version":"1"}}),
        )?;
        writeln!(
            result.input,
            "{}",
            json!({"method":"initialized","params":{}})
        )?;
        result.input.flush()?;
        Ok(result)
    }
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.ident += 1;
        writeln!(
            self.input,
            "{}",
            json!({"id":self.ident,"method":method,"params":params})
        )?;
        self.input.flush()?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let row = self
                .rows
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| error("Codex authentication broker timed out or disconnected"))?;
            if row["id"].as_u64() != Some(self.ident) {
                continue;
            }
            if row.get("error").is_some() {
                return Err(error(format!(
                    "Codex does not support the required authentication RPC: {method}"
                )));
            }
            return Ok(row.get("result").cloned().unwrap_or_else(|| json!({})));
        }
    }
    pub fn login(&mut self, refresh: bool) -> Result<()> {
        if !refresh
            && self
                .cached
                .as_ref()
                .is_some_and(|v| v.at.elapsed() < Duration::from_secs(60))
        {
            return Ok(());
        }
        let refresh_name = concat!("refresh", "To", "ken");
        let account = self.call("account/read", json!({refresh_name:refresh}))?;
        if account["account"]["type"] != "chatgpt" {
            return Err(error(
                "An existing Codex ChatGPT subscription login is required",
            ));
        }
        let include_name = concat!("include", "To", "ken");
        let exported_name = concat!("auth", "To", "ken");
        let mut response = self.call(
            "getAuthStatus",
            json!({include_name:true,refresh_name:false}),
        )?;
        if response["authMethod"] != "chatgpt" {
            return Err(error(
                "Codex cannot export the current subscription credential",
            ));
        }
        let mut bearer = response
            .get_mut(exported_name)
            .map(Value::take)
            .and_then(|v| v.as_str().map(|s| s.as_bytes().to_vec()))
            .ok_or_else(|| error("Codex cannot export the current subscription credential"))?;
        let result = (|| {
            let text = std::str::from_utf8(&bearer)
                .map_err(|_| error("unsupported subscription credential"))?;
            let mut decoded = crate::messages::decode64(
                text.split('.')
                    .nth(1)
                    .ok_or_else(|| error("unsupported subscription credential"))?,
            )?;
            let claims = json(&decoded);
            decoded.fill(0);
            let claims = claims?;
            let account = claims["https://api.openai.com/auth"]["chatgpt_account_id"]
                .as_str()
                .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
                .ok_or_else(|| error("unsupported subscription account"))?
                .to_owned();
            if text.chars().any(char::is_control) {
                return Err(error("unsupported subscription credential"));
            }
            let expires = claims["exp"]
                .as_f64()
                .ok_or_else(|| error("unsupported subscription expiry"))?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            Ok((account, expires >= now + 120.0))
        })();
        let (account, valid) = match result {
            Ok(v) => v,
            Err(e) => {
                bearer.fill(0);
                return Err(e);
            }
        };
        if !valid {
            bearer.fill(0);
            if refresh {
                return Err(error(
                    "Codex subscription login expired; sign in again in Codex",
                ));
            }
            return self.login(true);
        }
        self.cached = Some(Login {
            bearer,
            account,
            at: Instant::now(),
        });
        Ok(())
    }
    pub fn model(&mut self, name: &str) -> Result<Value> {
        let mut cursor = Value::Null;
        for _ in 0..20 {
            let page = self.call("model/list", json!({"cursor":cursor,"limit":100}))?;
            if let Some(rows) = page["data"].as_array() {
                for row in rows {
                    if row["model"] == name || row["id"] == name {
                        return Ok(row.clone());
                    }
                }
            }
            cursor = page["nextCursor"].clone();
            if cursor.is_null() || cursor == "" {
                break;
            }
        }
        Err(error(format!(
            "The exact model is absent from the installed Codex catalog: {name}"
        )))
    }
    fn headers(&mut self, refresh: bool) -> Result<Vec<(String, String)>> {
        self.login(refresh)?;
        let current = self
            .cached
            .as_ref()
            .ok_or_else(|| error("missing subscription login"))?;
        Ok(vec![
            (
                "Authorization".into(),
                format!("Bearer {}", String::from_utf8_lossy(&current.bearer)),
            ),
            ("ChatGPT-Account-Id".into(), current.account.clone()),
            ("Content-Type".into(), "application/json".into()),
            ("Accept".into(), "text/event-stream".into()),
            ("OpenAI-Beta".into(), "responses=experimental".into()),
            ("originator".into(), "oms_harness".into()),
        ])
    }
}
impl Drop for Broker {
    fn drop(&mut self) {
        self.cached = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub(crate) trait Backend: Send + Sync {
    fn open(&self, body: &Value) -> Result<HttpsResponse>;
}
pub(crate) struct SubscriptionBackend {
    pub broker: Arc<Mutex<Broker>>,
    pub transport: Arc<dyn HttpsTransport>,
}
impl Backend for SubscriptionBackend {
    fn open(&self, body: &Value) -> Result<HttpsResponse> {
        for refresh in [false, true] {
            let headers = self
                .broker
                .lock()
                .map_err(|_| error("broker unavailable"))?
                .headers(refresh)?;
            let response = self.transport.open(&headers, body.to_string().as_bytes());
            for (_, value) in headers {
                let mut bytes = value.into_bytes();
                bytes.fill(0);
            }
            let response = response.map_err(|_| error("Codex HTTPS transport failed"))?;
            if response.status == 401 && !refresh {
                continue;
            }
            return Ok(response);
        }
        Err(error("Codex subscription authentication failed"))
    }
}
