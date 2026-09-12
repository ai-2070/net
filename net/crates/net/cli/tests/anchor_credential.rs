//! Stage 4b — `net-mesh anchor credential mint|inspect`, driven as
//! the real binary against a tempdir.
//!
//! What these hold, that the SDK unit tests cannot: the operator
//! surface. A minted credential is one a browser can actually parse;
//! the PSK never reaches stdout on the inspect path; a file written
//! with `--out` is not silently overwritten; and a credential for
//! another trust domain is *reported* as foreign rather than
//! accepted.

use assert_cmd::prelude::*;
use std::path::Path;
use std::process::Command;

const PSK_A: &str = "4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a4a";
const PSK_B: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
const ROOT: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const ANCHOR_KEY: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn mint(extra: &[&str]) -> std::process::Output {
    let mut cmd = Command::cargo_bin("net-mesh").unwrap();
    cmd.args([
        "--output",
        "json",
        "anchor",
        "credential",
        "mint",
        "--root",
        ROOT,
        "--anchor-noise-pubkey",
        ANCHOR_KEY,
        "--psk-hex",
        PSK_A,
        "--url",
        "https://anchor.example.com",
    ]);
    cmd.args(extra);
    cmd.output().unwrap()
}

fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn a_minted_credential_parses_back_and_states_both_lifetimes() {
    let out = mint(&["--invite-ttl-secs", "600", "--psk-ttl-secs", "86400"]);
    assert!(out.status.success(), "{:?}", out);
    let v = json(&out);
    let credential = v["credential"].as_str().expect("credential string");
    assert!(credential.starts_with("net-bootstrap:"));

    // The two deadlines are far apart and both present — the whole
    // point of the format carrying them separately.
    let nonce = v["nonce_expires_at"].as_u64().unwrap();
    let psk = v["psk_expires_at"].as_u64().unwrap();
    assert!(
        psk > nonce + 80_000,
        "the standing half must outlive the single-use half (nonce {nonce}, psk {psk})"
    );

    // …and a consumer can parse what the operator minted.
    let parsed =
        net_sdk::bootstrap_credential::BrowserBootstrapCredential::decode(credential).unwrap();
    assert_eq!(parsed.bootstrap_url, "https://anchor.example.com");
    assert_eq!(parsed.nonce_expires_at(), nonce);
    assert_eq!(parsed.psk_expires_at(), psk);
}

#[test]
fn inspect_prints_the_trust_domain_and_never_the_psk() {
    let minted = mint(&[]);
    let credential = json(&minted)["credential"].as_str().unwrap().to_string();

    let out = Command::cargo_bin("net-mesh")
        .unwrap()
        .args([
            "--output",
            "json",
            "anchor",
            "credential",
            "inspect",
            "--credential",
            &credential,
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        !stdout.contains(PSK_A),
        "inspect must never print the PSK: {stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(v.get("psk").is_none(), "no psk field exists on the report");
    assert_eq!(v["status"], "ok");
    assert_eq!(v["anchor_noise_pubkey"], ANCHOR_KEY);
    // The trust domain IS printed — it is one-way, and it is what an
    // operator needs to tell two domains apart.
    assert!(v["trust_domain"].as_str().unwrap().len() == 32);
}

#[test]
fn inspect_reports_a_foreign_trust_domain_rather_than_accepting_it() {
    let credential = json(&mint(&[]))["credential"].as_str().unwrap().to_string();
    let check = |psk: &str| -> serde_json::Value {
        let out = Command::cargo_bin("net-mesh")
            .unwrap()
            .args([
                "--output",
                "json",
                "anchor",
                "credential",
                "inspect",
                "--credential",
                &credential,
                "--psk-hex",
                psk,
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        serde_json::from_slice(&out.stdout).unwrap()
    };
    assert_eq!(check(PSK_A)["trust_domain_matches"], true);
    assert_eq!(check(PSK_B)["trust_domain_matches"], false);
}

#[test]
fn an_expired_credential_inspects_and_says_which_half_expired() {
    // A one-second invite: by the time we inspect it, the single-use
    // half is gone while the standing half is not. `inspect` must
    // still show the fields (that is what it is for) and name the
    // half that failed.
    let credential = json(&mint(&[
        "--invite-ttl-secs",
        "1",
        "--psk-ttl-secs",
        "86400",
    ]))["credential"]
        .as_str()
        .unwrap()
        .to_string();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let out = Command::cargo_bin("net-mesh")
        .unwrap()
        .args([
            "--output",
            "json",
            "anchor",
            "credential",
            "inspect",
            "--credential",
            &credential,
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let status = v["status"].as_str().unwrap();
    assert!(
        status.contains("nonce expired"),
        "the status must name the single-use half: {status}"
    );
    assert_eq!(v["bootstrap_url"], "https://anchor.example.com");
}

#[test]
fn writing_a_credential_file_refuses_to_overwrite_without_force() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("browser.cred");
    let first = mint(&["--out", path.to_str().unwrap()]);
    assert!(first.status.success(), "{:?}", first);
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.starts_with("net-bootstrap:"));
    assert_eq!(json(&first)["credential"].as_str().unwrap(), written);

    let second = mint(&["--out", path.to_str().unwrap()]);
    assert!(
        !second.status.success(),
        "a second mint must not clobber a credential file"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), written);

    let forced = mint(&["--out", path.to_str().unwrap(), "--force"]);
    assert!(forced.status.success(), "{:?}", forced);
    assert_ne!(
        std::fs::read_to_string(&path).unwrap(),
        written,
        "--force mints a fresh nonce, so the file must actually change"
    );
    assert!(!has_stage_temp(dir.path()), "no staging temp left behind");
}

#[test]
fn a_url_no_browser_could_fetch_is_refused_at_mint_time() {
    let out = Command::cargo_bin("net-mesh")
        .unwrap()
        .args([
            "anchor",
            "credential",
            "mint",
            "--root",
            ROOT,
            "--anchor-noise-pubkey",
            ANCHOR_KEY,
            "--psk-hex",
            PSK_A,
            "--url",
            "http://anchor.example.com",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "plain http off localhost is not a secure context; minting it defers the \
         failure to the browser"
    );
}

#[test]
fn minting_without_a_psk_is_refused() {
    let out = Command::cargo_bin("net-mesh")
        .unwrap()
        .args([
            "anchor",
            "credential",
            "mint",
            "--root",
            ROOT,
            "--anchor-noise-pubkey",
            ANCHOR_KEY,
            "--url",
            "https://anchor.example.com",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        stderr.contains("psk"),
        "the error must name the PSK: {stderr}"
    );
}

#[test]
fn the_psk_may_come_from_a_file_instead_of_argv() {
    let dir = tempfile::tempdir().unwrap();
    let psk_path = dir.path().join("domain.psk");
    std::fs::write(&psk_path, format!("{PSK_A}\n")).unwrap();
    let out = Command::cargo_bin("net-mesh")
        .unwrap()
        .args([
            "--output",
            "json",
            "anchor",
            "credential",
            "mint",
            "--root",
            ROOT,
            "--anchor-noise-pubkey",
            ANCHOR_KEY,
            "--psk-file",
            psk_path.to_str().unwrap(),
            "--url",
            "https://anchor.example.com",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    // Same PSK, so the same trust domain as the argv form.
    let from_file = json(&out)["trust_domain"].as_str().unwrap().to_string();
    let from_argv = json(&mint(&[]))["trust_domain"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(from_file, from_argv);
}

/// True if any `*.stage.*` publish temp was left behind in `dir`.
fn has_stage_temp(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().contains(".stage."))
}
