//! The runner's handle on one namespace's browser driver
//! (`driver/driver.mjs`).
//!
//! One `node` child per NAT'd namespace, launched through
//! `ip netns exec`, NDJSON on its stdio, one request at a time,
//! correlated by id. Everything the driver prints on stderr is
//! forwarded with the namespace's label so a page console line and a
//! harness line land in the same scenario log.
//!
//! This module is the only place the runner knows what a browser
//! process is, and `ip netns exec` is the only line that knows where
//! it runs.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// How long any single driver request may take. `open` navigates a
/// real page in a real browser inside a NAT'd namespace; the rest are
/// milliseconds.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Driver {
    label: String,
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

/// What the engine's own port allocator did about interface
/// enumeration, as the driver counted it over the browser's whole
/// lifetime.
///
/// The §6.12 signature is `real == 0 && wildcard > 0`: zero
/// enumerated interfaces, ports bound to the `any` address, and every
/// inbound datagram dropped before STUN parsing. `run_row` refuses a
/// Chromium row that reports it.
#[derive(Debug, Default, Clone)]
pub struct Networks {
    /// Ports allocated on a real, named network (`Net[eth0:…]`).
    pub real: u64,
    /// Ports allocated on the wildcard `any` network.
    pub wildcard: u64,
    /// The network descriptors seen, verbatim and deduplicated.
    pub nets: Vec<String>,
}

impl Driver {
    /// Launch the driver INSIDE `netns`.
    ///
    /// `ip netns exec` from this process (which is itself inside
    /// `nsim_wan`) works because `setns` is a capability question, not
    /// a topology one, and the script already runs as root. The child
    /// keeps the stdio pipes created here, so the control channel
    /// crosses the namespace boundary while every socket the driver,
    /// Playwright and the browser open does not.
    ///
    /// `home` and `runtime` are a ROOT-owned pair, and Firefox needs
    /// them. The rows run as root (netns + nft), and Firefox refuses
    /// to start at all when it is root while `$XDG_RUNTIME_DIR`
    /// belongs to somebody else — on the GitHub runner that is
    /// `/run/user/1001`, owned by `runner`, inherited straight
    /// through `sudo`. It exits 1 with
    /// `Running Nightly as root in a regular user's session is not
    /// supported.`, which Playwright surfaces as the far less
    /// informative `Target page, context or browser has been closed`.
    /// Chromium does not care, so this is set unconditionally rather
    /// than per engine: one environment for both engines is one thing
    /// to reason about.
    pub async fn spawn(
        netns: &str,
        driver_dir: &Path,
        label: &str,
        home: &Path,
        runtime: &Path,
    ) -> Result<Self, String> {
        let mut child = Command::new("ip")
            .args(["netns", "exec", netns, "node", "driver.mjs"])
            .current_dir(driver_dir)
            .env("HOME", home)
            .env("XDG_RUNTIME_DIR", runtime)
            .env("NATSIM_NETNS", netns)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn driver in {netns}: {e} (is iproute2 installed?)"))?;
        let stdin = child.stdin.take().ok_or("driver stdin")?;
        let stdout = child.stdout.take().ok_or("driver stdout")?;
        let stderr = child.stderr.take().ok_or("driver stderr")?;
        let tag = label.to_owned();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                println!("[{tag}] {line}");
            }
        });
        let mut driver = Self {
            label: label.to_owned(),
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
        };
        // The hello line, so "the driver never started" is reported
        // here rather than as a timeout on the first real request.
        let hello = driver.read_line().await?;
        if hello.get("hello").is_none() {
            return Err(format!(
                "{label}: driver did not greet: {hello} (npm ci / playwright install?)"
            ));
        }
        Ok(driver)
    }

    async fn read_line(&mut self) -> Result<serde_json::Value, String> {
        let line = tokio::time::timeout(REQUEST_TIMEOUT, self.lines.next_line())
            .await
            .map_err(|_| format!("{}: driver timed out", self.label))?
            .map_err(|e| format!("{}: driver stdout: {e}", self.label))?
            .ok_or_else(|| format!("{}: driver exited", self.label))?;
        serde_json::from_str(&line)
            .map_err(|e| format!("{}: unparseable driver line {line:?}: {e}", self.label))
    }

    async fn request(
        &mut self,
        op: &str,
        mut req: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        req["id"] = serde_json::json!(id);
        req["op"] = serde_json::json!(op);
        let mut buf = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
        buf.push(b'\n');
        self.stdin
            .write_all(&buf)
            .await
            .map_err(|e| format!("{}: driver stdin: {e}", self.label))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("{}: driver flush: {e}", self.label))?;
        let reply = self.read_line().await?;
        if reply.get("id").and_then(serde_json::Value::as_u64) != Some(id) {
            return Err(format!(
                "{}: driver replied out of order: {reply}",
                self.label
            ));
        }
        if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(format!(
                "{}: {op} failed: {}",
                self.label,
                reply
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("(no error field)")
            ));
        }
        Ok(reply)
    }

    /// Start the engine. `spki_pin` is Chromium's one-key pin;
    /// `ca_pem` is what Firefox's profile NSS database is seeded with.
    /// Which one an engine uses is the driver's business.
    pub async fn launch(
        &mut self,
        engine: &str,
        spki_pin: &str,
        ca_pem: &str,
        profile_dir: &Path,
    ) -> Result<String, String> {
        let reply = self
            .request(
                "launch",
                serde_json::json!({
                    "engine": engine,
                    "spkiPin": spki_pin,
                    "caPem": ca_pem,
                    "profileDir": profile_dir.to_string_lossy(),
                }),
            )
            .await?;
        Ok(reply
            .get("trust")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(unreported)")
            .to_owned())
    }

    /// Open the row's page, and report what the driver GRANTED it.
    ///
    /// `media` is `granted` or `none` and is the row's own field, not
    /// a runner preference: Chromium withholds interface enumeration
    /// from WebRTC until a media permission exists (§6.12), so the
    /// grant changes the networking environment the product runs in
    /// and a row that acquired it silently would be measuring a
    /// different environment under its own name.
    ///
    /// The returned string is what the DRIVER says it did, read back
    /// rather than echoed from the request, so a driver that ignored
    /// `media` fails the row instead of passing under the label the
    /// runner asked for.
    pub async fn open(&mut self, url: &str, media: &str) -> Result<String, String> {
        let reply = self
            .request("open", serde_json::json!({ "url": url, "media": media }))
            .await?;
        Ok(reply
            .get("media")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "{}: the driver did not report what it granted the page — an unreported \
                     grant is indistinguishable from none",
                    self.label
                )
            })?
            .to_owned())
    }

    /// Stop the engine, and report what it enumerated.
    ///
    /// The counters come back in the `shutdown` reply rather than
    /// being parsed out of the log by a second reader: the driver is
    /// already counting them line by line, and a number the runner
    /// can assert on is worth more than one a human can find.
    pub async fn shutdown(mut self) -> Networks {
        let reply = self.request("shutdown", serde_json::json!({})).await;
        drop(self.stdin);
        let _ = tokio::time::timeout(Duration::from_secs(20), self.child.wait()).await;
        let Ok(reply) = reply else {
            return Networks::default();
        };
        let networks = &reply["networks"];
        Networks {
            real: networks["real"].as_u64().unwrap_or(0),
            wildcard: networks["wildcard"].as_u64().unwrap_or(0),
            nets: networks["nets"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|n| n.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}
