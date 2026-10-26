//! Provision the logger for a Cairn server, over USB.
//!
//! The device id is the dongle's rule: the first 16 bytes of SHA-256 of the
//! Wi-Fi MAC, as 32 lowercase hex. The server refuses a bundle whose manifest
//! device id differs from the client certificate's CommonName, so the
//! certificate is issued for exactly that name.
//!
//! Inputs, none in the repository:
//!   ~/cairn-backups/pki-20261007/ca.pem, ca-key.pem   the Cairn private CA (key only on this Mac)
//!   ~/.config/cairn/wifi.env                          WIFI_SSID, WIFI_PASSWORD, optional WIFI_BSSID / WIFI_CHANNEL
//! Outputs:
//!   ~/cairn-backups/trail-<device_id>/                device key (0600), CSR, certificate

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::config;
use crate::serial::Port;

const SERVER_HOST: &str = "cairn.alpina.casa";
const SERVER_IP: [u8; 4] = [172, 16, 6, 80];
const SERVER_PORT: u16 = 8443;

mod tag {
    pub const DEVICE_ID: u8 = 1;
    pub const CLIENT_CERT: u8 = 2;
    pub const CLIENT_KEY: u8 = 3;
    pub const CA_CERT: u8 = 4;
    pub const WIFI_SSID: u8 = 5;
    pub const WIFI_PASSWORD: u8 = 6;
    pub const WIFI_BSSID: u8 = 7;
    pub const WIFI_CHANNEL: u8 = 8;
    pub const SERVER_HOST: u8 = 9;
    pub const SERVER_IP: u8 = 10;
    pub const SERVER_PORT: u8 = 11;
    pub const RECEIPT_KEY: u8 = 12;
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Issue the device's TLS client certificate from the Cairn CA.
    Cert {
        #[arg(long)]
        mac: String,
        #[arg(long)]
        force: bool,
    },
    /// Try that certificate against the server, from this computer.
    Test {
        #[arg(long)]
        mac: String,
    },
    /// Write identity, Wi-Fi and server into the board's flash.
    Write {
        #[arg(long)]
        mac: String,
        #[arg(long)]
        port: Option<String>,
        /// Join only the access point named in wifi.env.
        #[arg(long)]
        pin_bssid: bool,
    },
}

fn pki() -> PathBuf {
    config::home().join("cairn-backups/pki-20261007")
}

fn openssl() -> Result<&'static str> {
    [
        "/opt/homebrew/opt/openssl@3/bin/openssl",
        "/opt/homebrew/opt/openssl@3.5/bin/openssl",
    ]
    .into_iter()
    .find(|p| Path::new(p).exists())
    .context("needs Homebrew OpenSSL 3 (macOS LibreSSL signs with SHA-1, which Go rejects)")
}

fn run(prog: &str, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new(prog).args(args).output()?;
    if !out.status.success() {
        bail!(
            "{prog} {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out.stdout)
}

pub fn device_id(mac: &str) -> Result<String> {
    let raw = hex::decode(mac.replace([':', '-'], "")).context("MAC is not hex")?;
    if raw.len() != 6 {
        bail!("MAC must be 6 bytes");
    }
    Ok(hex::encode(&Sha256::digest(&raw)[..16]))
}

fn outdir(id: &str) -> Result<PathBuf> {
    let d = config::home().join(format!("cairn-backups/trail-{id}"));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

pub fn run_cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Cert { mac, force } => cert(&mac, force),
        Cmd::Test { mac } => test(&mac),
        Cmd::Write {
            mac,
            port,
            pin_bssid,
        } => write(&mac, port.as_deref(), pin_bssid),
    }
}

fn cert(mac: &str, force: bool) -> Result<()> {
    let ssl = openssl()?;
    let id = device_id(mac)?;
    let d = outdir(&id)?;
    let (key, csr, crt) = (
        d.join("client-key.pem"),
        d.join("client.csr"),
        d.join("client.pem"),
    );
    if crt.exists() && !force {
        println!("{} exists (--force to reissue)", crt.display());
        return Ok(());
    }
    let pem = run(
        ssl,
        &["ecparam", "-name", "prime256v1", "-genkey", "-noout"],
    )?;
    config::save_private(&key, &pem)?;
    let (k, c, t) = (
        key.to_str().unwrap(),
        csr.to_str().unwrap(),
        crt.to_str().unwrap(),
    );
    run(
        ssl,
        &[
            "req",
            "-new",
            "-key",
            k,
            "-subj",
            &format!("/CN={id}"),
            "-out",
            c,
            "-sha256",
        ],
    )?;
    let ext = std::env::temp_dir().join(format!("trail-ext-{}.cnf", std::process::id()));
    std::fs::write(
        &ext,
        "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=clientAuth\n",
    )?;
    let p = pki();
    let r = run(
        ssl,
        &[
            "x509",
            "-req",
            "-in",
            c,
            "-CA",
            p.join("ca.pem").to_str().unwrap(),
            "-CAkey",
            p.join("ca-key.pem").to_str().unwrap(),
            "-CAserial",
            p.join("ca.srl").to_str().unwrap(),
            "-out",
            t,
            "-days",
            "3650",
            "-sha256",
            "-extfile",
            ext.to_str().unwrap(),
        ],
    );
    let _ = std::fs::remove_file(&ext);
    r?;
    println!(
        "issued CN={id}\n  {}\n  {} (0600)",
        crt.display(),
        key.display()
    );
    print!(
        "{}",
        String::from_utf8_lossy(&run(
            ssl,
            &["x509", "-in", t, "-noout", "-subject", "-issuer", "-dates"]
        )?)
    );
    Ok(())
}

/// An agent that presents the device's certificate and trusts only the Cairn CA.
fn mtls_agent(id: &str) -> Result<ureq::Agent> {
    use ureq::tls::{Certificate, ClientCert, PrivateKey, RootCerts, TlsConfig};
    let d = outdir(id)?;
    let ca = Certificate::from_pem(&std::fs::read(pki().join("ca.pem"))?)?;
    let cert = Certificate::from_pem(
        &std::fs::read(d.join("client.pem")).context("run `trail provision cert` first")?,
    )?;
    let key = PrivateKey::from_pem(&std::fs::read(d.join("client-key.pem"))?)?;
    let tls = TlsConfig::builder()
        .root_certs(RootCerts::Specific(Arc::new(vec![ca])))
        .client_cert(Some(ClientCert::new_with_certs(&[cert], key)))
        .build();
    Ok(ureq::Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into())
}

/// The server's certificate names its LAN address too, so it can be reached
/// by IP without DNS.
fn server_url(path: &str) -> String {
    let [a, b, c, d] = SERVER_IP;
    format!("https://{a}.{b}.{c}.{d}:{SERVER_PORT}{path}")
}

fn test(mac: &str) -> Result<()> {
    let id = device_id(mac)?;
    let mut r = mtls_agent(&id)?.get(&server_url("/api/v2/health")).call()?;
    let status = r.status().as_u16();
    println!("{}\nHTTP {status}", r.body_mut().read_to_string()?);
    Ok(())
}

fn receipt_key(id: &str) -> Option<(Vec<u8>, String)> {
    let mut r = mtls_agent(id)
        .ok()?
        .get(&server_url("/api/v2/server/receipt-key"))
        .call()
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&r.body_mut().read_to_string().ok()?).ok()?;
    Some((
        hex::decode(v["public_key"].as_str()?).ok()?,
        v["algorithm"].as_str().unwrap_or("?").into(),
    ))
}

fn der(ssl: &str, kind: &str, path: &Path) -> Result<Vec<u8>> {
    run(
        ssl,
        &[kind, "-in", path.to_str().unwrap(), "-outform", "DER"],
    )
}

fn payload(mac: &str, pin_bssid: bool) -> Result<Vec<u8>> {
    let ssl = openssl()?;
    let id = device_id(mac)?;
    let d = outdir(&id)?;
    if !d.join("client.pem").exists() {
        bail!("run `trail provision cert` first");
    }
    let w = config::load_env("wifi.env")?;
    let mut p = Vec::new();
    let mut entry = |t: u8, v: &[u8]| {
        p.push(t);
        p.extend_from_slice(&(v.len() as u16).to_le_bytes());
        p.extend_from_slice(v);
    };
    entry(tag::DEVICE_ID, &hex::decode(&id)?);
    entry(tag::CLIENT_CERT, &der(ssl, "x509", &d.join("client.pem"))?);
    entry(tag::CLIENT_KEY, &der(ssl, "ec", &d.join("client-key.pem"))?);
    entry(tag::CA_CERT, &der(ssl, "x509", &pki().join("ca.pem"))?);
    entry(
        tag::WIFI_SSID,
        w.get("WIFI_SSID").context("WIFI_SSID missing")?.as_bytes(),
    );
    entry(
        tag::WIFI_PASSWORD,
        w.get("WIFI_PASSWORD")
            .map(String::as_bytes)
            .unwrap_or_default(),
    );
    entry(tag::SERVER_HOST, SERVER_HOST.as_bytes());
    entry(tag::SERVER_IP, &SERVER_IP);
    entry(tag::SERVER_PORT, &SERVER_PORT.to_le_bytes());
    if pin_bssid && let Some(b) = w.get("WIFI_BSSID") {
        entry(tag::WIFI_BSSID, &hex::decode(b.replace(':', ""))?);
        if let Some(c) = w.get("WIFI_CHANNEL") {
            entry(tag::WIFI_CHANNEL, &[c.parse::<u8>()?]);
        }
    }
    match receipt_key(&id) {
        Some((k, alg)) => {
            println!("server receipt key ({alg}): {}…", &hex::encode(&k)[..16]);
            entry(tag::RECEIPT_KEY, &k);
        }
        None => println!(
            "warning: could not fetch the server's receipt key; receipts will not verify until it is provisioned"
        ),
    }
    Ok(p)
}

fn write(mac: &str, port: Option<&str>, pin_bssid: bool) -> Result<()> {
    let p = payload(mac, pin_bssid)?;
    let mut port = Port::open(port, Duration::from_secs(5))?;
    println!("writing {} bytes to {} ...", p.len(), port.name());
    let r = port.upload("prov", &p, true)?;
    println!("{r}");
    if !r.contains("written") {
        bail!("the board did not take it");
    }
    println!(
        "{}",
        port.ask("prov show", "prov: mac=", &[], Duration::from_secs(3))?
    );
    println!("reboot the board (`trail console --cmd reboot`) to start Wi-Fi");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn device_id_matches_the_issued_certificate() {
        assert_eq!(
            super::device_id("48:ca:43:5b:ed:60").unwrap(),
            "f8c777eac1752d6fc2cee40d584d24bd"
        );
    }
}
