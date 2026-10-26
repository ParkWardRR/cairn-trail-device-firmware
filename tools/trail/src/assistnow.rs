//! u-blox AssistNow: register the board's GNSS receiver with Thingstream,
//! download assistance data, and put it on the card — over USB, or by copying
//! to the card in a reader — so the receiver gets a fix in seconds.
//!
//! Credentials come from ~/.config/cairn/thingstream.env (0600, never in the
//! repository):
//!
//!     ASSISTNOW_ZTP_URL=https://api.thingstream.io/ztp/assistnow/credentials
//!     ASSISTNOW_ZTP_TOKEN=<the ZTP token from the Thingstream device profile>
//!
//! What registration returns (the chipcode, service URLs) is kept in
//! ~/.config/cairn/assistnow.json, also 0600. The chipcode is bound to the
//! receiver chip, and the evaluation plan allows three devices: register only
//! frames read from a real board.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::config;
use crate::serial::Port;

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Read the receiver's identity frames over USB.
    Identity {
        #[arg(long)]
        port: Option<String>,
    },
    /// Register the receiver with Thingstream ZTP (once per board).
    Register {
        /// Send payload hex instead of whole frames.
        #[arg(long)]
        payload_only: bool,
    },
    /// Download AssistNow data to assist/ASSIST.UBX.
    Fetch {
        /// Service URL, if the registration response does not name one.
        #[arg(long)]
        url: Option<String>,
        /// Extra query parameter key=value (repeatable), e.g. gnss=gps,gal.
        #[arg(long = "param")]
        params: Vec<String>,
        #[arg(long, default_value = "assist/ASSIST.UBX")]
        out: PathBuf,
    },
    /// Send ASSIST.UBX to the board over USB; it writes it to the card and
    /// feeds the receiver at once.
    Upload {
        #[arg(long)]
        port: Option<String>,
        #[arg(long, default_value = "assist/ASSIST.UBX")]
        src: PathBuf,
    },
    /// Copy ASSIST.UBX to a card mounted on this computer.
    Install {
        card: PathBuf,
        #[arg(long, default_value = "assist/ASSIST.UBX")]
        src: PathBuf,
    },
}

const SECRET_HINTS: [&str; 6] = [
    "chipcode",
    "token",
    "secret",
    "password",
    "key",
    "credential",
];

fn identity_path() -> PathBuf {
    config::dir().join("gnss-identity.json")
}
fn creds_path() -> PathBuf {
    config::dir().join("assistnow.json")
}

/// A response's shape without its secrets.
fn redact(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    let secret =
                        v.is_string() && SECRET_HINTS.iter().any(|h| k.to_lowercase().contains(h));
                    (
                        k.clone(),
                        if secret {
                            json!("<redacted>")
                        } else {
                            redact(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact).collect()),
        other => other.clone(),
    }
}

/// Every string leaf whose key satisfies `pred`.
fn find_strings<'a>(v: &'a Value, pred: &dyn Fn(&str, &str) -> bool, out: &mut Vec<&'a str>) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                match v {
                    Value::String(s) if pred(&k.to_lowercase(), s) => out.push(s),
                    _ => find_strings(v, pred, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|v| find_strings(v, pred, out)),
        _ => {}
    }
}

/// Well-formed UBX frames by (class, id); empty if none.
pub fn summarize(data: &[u8]) -> BTreeMap<(u8, u8), usize> {
    let mut counts = BTreeMap::new();
    let mut parser = trail_core::ubx::Parser::new();
    for &b in data {
        if let Some(f) = parser.push(b) {
            *counts.entry((f.class, f.id)).or_insert(0) += 1;
        }
    }
    counts
}

pub fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Identity { port } => identity(port.as_deref()),
        Cmd::Register { payload_only } => register(payload_only),
        Cmd::Fetch { url, params, out } => fetch(url, &params, &out),
        Cmd::Upload { port, src } => upload(port.as_deref(), &src),
        Cmd::Install { card, src } => install(&card, &src),
    }
}

fn identity(port: Option<&str>) -> Result<()> {
    let mut p = Port::open(port, Duration::from_secs(5))?;
    let ver = p.ask("gnssid", "UBX-MON-VER ", &[], Duration::from_secs(4))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut uniq = None;
    while let Some(l) = p.line(deadline)? {
        if l.starts_with("UBX-SEC-UNIQID ") {
            uniq = Some(l);
            break;
        }
    }
    let ver = ver
        .split_once(' ')
        .map(|x| x.1.trim().to_string())
        .unwrap_or_default();
    let uniq = uniq
        .and_then(|l| l.split_once(' ').map(|x| x.1.trim().to_string()))
        .context("the board did not report UBX-SEC-UNIQID")?;
    for (k, v) in [("UBX-MON-VER", &ver), ("UBX-SEC-UNIQID", &uniq)] {
        if !(v.starts_with("b562") && v.len() % 2 == 0 && v.len() > 16) {
            bail!(
                "{k} is not a UBX frame (firmware too old, or the receiver did not answer): {v:.40}"
            );
        }
    }
    let ident = json!({"UBX-MON-VER": ver, "UBX-SEC-UNIQID": uniq});
    config::save_private(
        &identity_path(),
        serde_json::to_string_pretty(&ident)?.as_bytes(),
    )?;
    println!("UBX-MON-VER    {} bytes", ver.len() / 2);
    println!("UBX-SEC-UNIQID {uniq}");
    println!("saved to {}", identity_path().display());
    Ok(())
}

fn register(payload_only: bool) -> Result<()> {
    let env = config::load_env("thingstream.env")?;
    let ident: Value = serde_json::from_slice(
        &std::fs::read(identity_path())
            .context("run `trail assistnow identity` first (board plugged in)")?,
    )?;
    let mut messages = serde_json::Map::new();
    for k in ["UBX-MON-VER", "UBX-SEC-UNIQID"] {
        let v = ident[k].as_str().context("identity file incomplete")?;
        // Payload only: drop sync, class, id, length (6 bytes) and checksum (2).
        let v = if payload_only { &v[12..v.len() - 4] } else { v };
        messages.insert(k.into(), json!(v));
    }
    let url = env
        .get("ASSISTNOW_ZTP_URL")
        .context("ASSISTNOW_ZTP_URL missing")?;
    let token = env
        .get("ASSISTNOW_ZTP_TOKEN")
        .context("ASSISTNOW_ZTP_TOKEN missing")?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut resp = agent
        .post(url)
        .send_json(json!({"token": token, "messages": messages}))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string()?;
    if status != 200 {
        bail!("ZTP refused (HTTP {status}): {text:.500}");
    }
    let v: Value = serde_json::from_str(&text)?;
    config::save_private(&creds_path(), serde_json::to_string_pretty(&v)?.as_bytes())?;
    println!(
        "registered; response saved to {} (0600). Its shape:",
        creds_path().display()
    );
    println!("{}", serde_json::to_string_pretty(&redact(&v))?);
    Ok(())
}

fn fetch(url: Option<String>, params: &[String], out: &Path) -> Result<()> {
    let creds: Value = serde_json::from_slice(
        &std::fs::read(creds_path()).context("run `trail assistnow register` first")?,
    )?;
    let mut chips = Vec::new();
    find_strings(&creds, &|k, _| k.contains("chipcode"), &mut chips);
    let chip = *chips.first().with_context(|| {
        format!(
            "no chipcode in {}; its shape:\n{}",
            creds_path().display(),
            redact(&creds)
        )
    })?;
    let url = match url {
        Some(u) => u,
        None => {
            let mut urls = Vec::new();
            find_strings(&creds, &|_, v| v.starts_with("http"), &mut urls);
            urls.first()
                .context("the registration response names no service URL; pass --url")?
                .to_string()
        }
    };
    let mut q = vec![("chipcode".to_string(), chip.to_string())];
    for kv in params {
        let (k, v) = kv.split_once('=').context("--param wants key=value")?;
        q.push((k.into(), v.into()));
    }
    let query: Vec<String> = q
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect();
    let full = format!(
        "{url}{}{}",
        if url.contains('?') { "&" } else { "?" },
        query.join("&")
    );
    println!("GET {}", full.replace(chip, "<chipcode>"));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();
    let mut resp = agent.get(&full).call()?;
    let status = resp.status().as_u16();
    let data = resp
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec()?;
    if status != 200 {
        bail!(
            "HTTP {status}: {}",
            String::from_utf8_lossy(&data[..data.len().min(300)])
        );
    }
    let summary = summarize(&data);
    if summary.is_empty() {
        bail!(
            "the reply is not UBX data ({} bytes): {}",
            data.len(),
            String::from_utf8_lossy(&data[..data.len().min(200)])
        );
    }
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, &data)?;
    println!(
        "{} bytes, {} UBX frames -> {}",
        data.len(),
        summary.values().sum::<usize>(),
        out.display()
    );
    for ((c, i), n) in &summary {
        println!("  class 0x{c:02x} id 0x{i:02x}: {n}{}", describe(*c, *i));
    }
    Ok(())
}

fn describe(class: u8, id: u8) -> &'static str {
    match (class, id) {
        (0x13, 0x00) => "  MGA-GPS (ephemeris, almanac, ionosphere, UTC)",
        (0x13, 0x02) => "  MGA-GAL",
        (0x13, 0x03) => "  MGA-BDS",
        (0x13, 0x05) => "  MGA-QZSS",
        (0x13, 0x06) => "  MGA-GLO",
        (0x13, 0x20) => "  MGA-ANO (AssistNow Offline)",
        (0x13, 0x40) => {
            "  MGA-INI (time/position when made; the logger drops these and primes its own)"
        }
        _ => "",
    }
}

/// Percent-encode a query value, keeping the separators AssistNow uses.
fn encode(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b',' | b';' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn upload(port: Option<&str>, src: &Path) -> Result<()> {
    let data = std::fs::read(src).with_context(|| format!("read {}", src.display()))?;
    if summarize(&data).is_empty() {
        bail!("{} holds no UBX frames", src.display());
    }
    let mut p = Port::open(port, Duration::from_secs(5))?;
    println!("sending {} bytes to {} ...", data.len(), p.name());
    let r = p.upload("assist", &data, true)?;
    println!("{r}");
    if !r.contains("written") {
        bail!("the board did not take it");
    }
    Ok(())
}

fn install(card: &Path, src: &Path) -> Result<()> {
    if !card.is_dir() {
        bail!("{} is not a mounted card", card.display());
    }
    let dest = card.join("ASSIST.UBX");
    std::fs::copy(src, &dest)?;
    println!(
        "copied {} -> {}; eject the card, put it in the logger, power on",
        src.display(),
        dest.display()
    );
    Ok(())
}
