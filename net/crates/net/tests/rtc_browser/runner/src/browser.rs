//! The runner's handle on the Playwright driver (`driver/driver.mjs`).
//!
//! One `node` child process, NDJSON on its stdio, one request at a
//! time, correlated by id. Everything the driver prints on stderr is
//! forwarded as `[driver] …` so a browser console line and a harness
//! line land in the same log the witnesses are read from.
//!
//! This module is the ONLY place the harness knows what a browser
//! process is. The witnesses below it see `open_page`, `close_page`,
//! `eval` and `freeze_page`; whether that is Chromium, Firefox or
//! WebKit is one field in one struct.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

/// Which engine the run drives. Chromium and Firefox are gates;
/// WebKit is best effort and recorded as such.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Chromium,
    Firefox,
    Webkit,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chromium => "chromium",
            Self::Firefox => "firefox",
            Self::Webkit => "webkit",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "chromium" | "chrome" => Some(Self::Chromium),
            "firefox" | "ff" => Some(Self::Firefox),
            "webkit" | "safari" => Some(Self::Webkit),
            _ => None,
        }
    }

    /// Is a failure on this engine a gate, or evidence?
    ///
    /// WebKit is the recorded best-effort leg: Playwright's WebKit is
    /// not Safari, its WebRTC stack differs from the shipping one,
    /// and it has no mDNS-obfuscation knob. A run that cannot drive
    /// it says so in the ledger instead of claiming Safari coverage.
    pub fn is_gate(self) -> bool {
        !matches!(self, Self::Webkit)
    }
}

/// Everything a launch needs. Cloned and mutated by one field for
/// the Stage 5 UDP profile, which relaunches the same browser with
/// its non-proxied UDP disabled.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub engine: Engine,
    /// The launch profile directory. Wiped and recreated per launch.
    pub profile: PathBuf,
    pub ca_pem: PathBuf,
    pub ca_nickname: String,
    /// Turn the engine's host-address obfuscation OFF. Only ever set
    /// deliberately — the default ON is what the mDNS witness
    /// measures.
    pub disable_mdns: bool,
    pub executable: Option<String>,
    /// `base64(SHA-256(SPKI))` of the harness leaf, for the engines
    /// that can trust exactly one key without a trust store —
    /// Chromium; see the runner's TLS doc.
    pub spki_pin: Option<String>,
    /// Take the engine's **non-proxied UDP** away: Chromium's
    /// `--force-webrtc-ip-handling-policy=disable_non_proxied_udp`
    /// (and Firefox's `media.peerconnection.ice.proxy_only`), with no
    /// proxy configured. The engine then gathers no UDP candidate at
    /// all — measured: zero candidates, not even a host one — while
    /// HTTPS is untouched. That is the UDP-blocked profile on a host
    /// whose kernel firewall cannot filter same-host traffic; see
    /// `udp_block.rs`.
    pub webrtc_udp_off: bool,
}

/// What `launch` reports back about the engine it started.
#[derive(Debug, Clone)]
pub struct Launched {
    pub version: String,
    /// How the harness CA reached this engine's trust store, or which
    /// key it was pinned to.
    pub trust: String,
    /// Was the engine's host-address obfuscation left ON?
    pub mdns_obfuscation: bool,
    /// What the engine was told about UDP — the exact flag or pref,
    /// so a ledger line can name the mechanism it measured.
    pub udp: String,
}

/// One TLS verification fact, read in a throwaway browser: did a
/// navigation to the harness listener complete, or did the engine
/// refuse the certificate?
#[derive(Debug, Clone)]
pub struct TlsProbe {
    /// `true` when the navigation returned an HTTP status — the
    /// engine verified the listener's certificate.
    pub verified: bool,
    /// The HTTP status, or the engine's own error text
    /// (`net::ERR_CERT_AUTHORITY_INVALID` and friends).
    pub detail: String,
}

pub struct Driver {
    child: Child,
    stdin: Mutex<ChildStdin>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: AtomicU64,
    dir: PathBuf,
}

impl Driver {
    /// Spawn the driver, installing its single dependency first if
    /// this checkout has not got it yet.
    pub async fn spawn(dir: &Path) -> Result<Self, String> {
        if !dir
            .join("node_modules/playwright-core/package.json")
            .exists()
        {
            println!("[harness] installing the driver's playwright-core (one time)");
            let out = Command::new(npm())
                .current_dir(dir)
                .args(["install", "--no-audit", "--no-fund"])
                .output()
                .await
                .map_err(|e| format!("npm: {e} (the merged runner needs Node >= 20 on PATH)"))?;
            if !out.status.success() {
                return Err(format!(
                    "npm install in {} failed: {}",
                    dir.display(),
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
        }

        let mut child = Command::new("node")
            .current_dir(dir)
            .arg("driver.mjs")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("node: {e} (the merged runner needs Node >= 20 on PATH)"))?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let reader_pending = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    println!("[driver] unparseable reply: {line}");
                    continue;
                };
                let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
                if id == 0 {
                    // The hello line, and any unsolicited notice.
                    println!("[driver] {line}");
                    continue;
                }
                if let Some(tx) = reader_pending.lock().await.remove(&id) {
                    let _ = tx.send(value);
                }
            }
        });

        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                println!("[driver] {line}");
            }
        });

        Ok(Self {
            child,
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            dir: dir.to_path_buf(),
        })
    }

    async fn request(&self, op: &str, mut body: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        body["id"] = json!(id);
        body["op"] = json!(op);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        {
            let mut w = self.stdin.lock().await;
            let line = format!("{body}\n");
            w.write_all(line.as_bytes())
                .await
                .map_err(|e| format!("driver stdin: {e}"))?;
            w.flush().await.map_err(|e| format!("driver flush: {e}"))?;
        }
        let reply = match tokio::time::timeout(Duration::from_secs(180), rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => return Err(format!("the driver dropped the `{op}` request")),
            Err(_) => return Err(format!("the driver did not answer `{op}` in 180 s")),
        };
        if reply.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(reply)
        } else {
            Err(reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the driver refused without a reason")
                .to_string())
        }
    }

    /// Make sure Playwright's own browser registry has this engine.
    ///
    /// A no-op when the revision is already in the cache, which is
    /// the CI case (the job installs it). Locally it is what makes
    /// `--engine firefox` work on a checkout that has only ever run
    /// Chromium. A failure is not fatal here: `launch` will say
    /// exactly which executable is missing, which is the better
    /// error.
    pub async fn ensure_engine(&self, engine: Engine) {
        let cli = self.dir.join("node_modules/playwright-core/cli.js");
        if !cli.exists() {
            return;
        }
        let out = Command::new("node")
            .current_dir(&self.dir)
            .arg(&cli)
            .args(["install", engine.as_str()])
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => println!(
                "[harness] playwright install {} said: {}",
                engine.as_str(),
                String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .last()
                    .unwrap_or("")
            ),
            Err(e) => println!("[harness] playwright install {}: {e}", engine.as_str()),
        }
    }

    /// Launch the run's browser from one spec.
    ///
    /// A struct rather than eight positional arguments because the
    /// Stage 5 UDP profile relaunches the SAME browser with exactly
    /// one field changed, and a positional call would make that
    /// difference invisible at the call site.
    pub async fn launch(&self, spec: &LaunchSpec) -> Result<Launched, String> {
        let reply = self
            .request(
                "launch",
                json!({
                    "engine": spec.engine.as_str(),
                    "profileDir": spec.profile.to_string_lossy(),
                    "caPemPath": spec.ca_pem.to_string_lossy(),
                    "caNickname": spec.ca_nickname,
                    "disableMdns": spec.disable_mdns,
                    "executablePath": spec.executable,
                    "spkiPin": spec.spki_pin,
                    "webrtcUdpOff": spec.webrtc_udp_off,
                }),
            )
            .await?;
        Ok(Launched {
            version: reply
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            trust: reply
                .get("trust")
                .and_then(Value::as_str)
                .unwrap_or("unreported")
                .to_string(),
            mdns_obfuscation: reply
                .get("mdnsObfuscation")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            udp: reply
                .get("udp")
                .and_then(Value::as_str)
                .unwrap_or("unreported")
                .to_string(),
        })
    }

    /// One throwaway browser, one navigation, one fact: did TLS
    /// verify?
    ///
    /// Separate from `launch` because it must run with a DIFFERENT
    /// trust configuration than the run's browser — that is the whole
    /// point — and because it must not disturb the live browser or its
    /// pages. The driver launches it, reads the outcome and closes it.
    pub async fn tls_probe(
        &self,
        engine: Engine,
        url: &str,
        spki_pin: Option<&str>,
        executable: Option<&str>,
        ca_pem: Option<&str>,
        ca_nickname: Option<&str>,
    ) -> Result<TlsProbe, String> {
        let reply = self
            .request(
                "tls_probe",
                json!({
                    "engine": engine.as_str(),
                    "url": url,
                    "spkiPin": spki_pin,
                    "executablePath": executable,
                    // Firefox's mechanism is a seeded profile, not a
                    // flag: `null` here is the control's "without".
                    "caPemPath": ca_pem,
                    "caNickname": ca_nickname,
                }),
            )
            .await?;
        Ok(TlsProbe {
            verified: reply
                .get("verified")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            detail: reply
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or("the driver reported no detail")
                .to_string(),
        })
    }

    pub async fn open_page(&self, name: &str, url: &str) -> Result<(), String> {
        self.request("open", json!({ "page": name, "url": url }))
            .await
            .map(|_| ())
    }

    pub async fn close_page(&self, name: &str) -> Result<(), String> {
        self.request("close_page", json!({ "page": name }))
            .await
            .map(|_| ())
    }

    pub async fn eval(&self, name: &str, expr: &str) -> Result<Value, String> {
        let reply = self
            .request("eval", json!({ "page": name, "expr": expr }))
            .await?;
        Ok(reply.get("value").cloned().unwrap_or(Value::Null))
    }

    /// Freeze or thaw a tab through CDP `Page.setWebLifecycleState`.
    /// Chromium only, and an error — never a no-op — anywhere else.
    pub async fn set_lifecycle(&self, name: &str, state: &str) -> Result<(), String> {
        self.request("lifecycle", json!({ "page": name, "state": state }))
            .await
            .map(|_| ())
    }

    pub async fn shutdown_browser(&self) -> Result<(), String> {
        self.request("shutdown", json!({})).await.map(|_| ())
    }

    /// Close the browser and the driver process.
    pub async fn quit(mut self) {
        let _ = self.shutdown_browser().await;
        {
            let mut w = self.stdin.lock().await;
            let _ = w.shutdown().await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await;
        let _ = self.child.start_kill();
    }
}

fn npm() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}
