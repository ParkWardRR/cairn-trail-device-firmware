//! Sealed `format/v3` bundles off the card, under their canonical member
//! names, and checked by the Cairn server's own verifier.
//!
//!     trail bundle pull B0000001 --out bundles/       # over USB, CRC-checked
//!     trail bundle verify bundles/<bundle_id>         # the server's cairn-verify
//!
//! On the card a bundle uses 8.3 names (`S0000000.SEG`, `JOURNAL.SEG`,
//! `MANIFEST.CBO`, `MANIFEST.SIG`, see firmware/src/bundle.rs); the manifest
//! names its members canonically (`seg-00000000.seg`, `journal.seg`), and
//! that is what a verifier and a server expect, so `pull` writes those.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::serial::Port;

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Pull a bundle directory off the card (e.g. B0000001).
    Pull {
        name: String,
        #[arg(long, default_value = "bundles")]
        out: PathBuf,
        #[arg(long)]
        port: Option<String>,
    },
    /// Check a pulled bundle with the server's cairn-verify.
    Verify {
        dir: PathBuf,
        /// The device's Ed25519 public key (console: `bundle`); asked for if omitted.
        #[arg(long)]
        device_key: Option<String>,
        /// The Cairn server checkout (for `go run ./cmd/cairn-verify`).
        #[arg(long, default_value = "../cairn-vehicle-server")]
        server: PathBuf,
        #[arg(long)]
        port: Option<String>,
    },
}

pub fn run(cmd: Cmd) -> Result<bool> {
    match cmd {
        Cmd::Pull { name, out, port } => pull(&name, &out, port.as_deref()).map(|_| true),
        Cmd::Verify {
            dir,
            device_key,
            server,
            port,
        } => verify(&dir, device_key, &server, port.as_deref()),
    }
}

/// The card's name for a member → its canonical name.
fn canonical(card: &str) -> Option<String> {
    match card {
        "JOURNAL.SEG" => Some("journal.seg".into()),
        "MANIFEST.CBO" => Some("manifest.cbor".into()),
        "MANIFEST.SIG" => Some("manifest.sig".into()),
        n if n.len() == 12 && n.starts_with('S') && n.ends_with(".SEG") => {
            let i: u32 = n[1..8].parse().ok()?;
            Some(cairn_format::bind::segment_member_name(i))
        }
        _ => None,
    }
}

/// The files `ls` lists under `dir/`.
fn list(p: &mut Port, dir: &str) -> Result<Vec<String>> {
    p.drain()?;
    p.send("ls")?;
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut files = Vec::new();
    let prefix = format!("{dir}/");
    while let Some(l) = p.line(deadline)? {
        if let Some(rest) = l
            .split_whitespace()
            .nth(1)
            .and_then(|f| f.strip_prefix(&prefix))
        {
            files.push(rest.to_string());
        }
    }
    Ok(files)
}

/// One file via `catx`, hex-decoded and checked against the CRC-32 the
/// firmware printed.
fn fetch(p: &mut Port, path: &str) -> Result<Vec<u8>> {
    p.drain()?;
    p.send(&format!("catx {path}"))?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let begin = format!("----- BEGIN {path} ");
    let end = format!("----- END {path} ");
    let mut inside = false;
    let mut bytes = Vec::new();
    while let Some(l) = p.line(deadline)? {
        if l.starts_with(&begin) {
            inside = true;
        } else if let Some(rest) = l.strip_prefix(&end) {
            // "<n> bytes crc32=xxxxxxxx -----"
            let w: Vec<&str> = rest.split_whitespace().collect();
            let n: usize = w.first().and_then(|s| s.parse().ok()).context("END line")?;
            let crc = w
                .get(2)
                .and_then(|s| s.strip_prefix("crc32="))
                .context("END line")?;
            let got = format!("{:08x}", crc32fast::hash(&bytes));
            if n != bytes.len() || got != crc {
                bail!(
                    "{path}: {} bytes crc32 {got}, the board says {n} bytes crc32 {crc}",
                    bytes.len()
                );
            }
            return Ok(bytes);
        } else if inside {
            bytes.extend(
                hex::decode(l.trim()).with_context(|| format!("{path}: not hex: {l:.40}"))?,
            );
        } else if l.starts_with("no such") {
            bail!("{l}");
        }
    }
    bail!("{path}: no END within 60 s")
}

fn pull(name: &str, out: &Path, port: Option<&str>) -> Result<()> {
    let mut p = Port::open(port, Duration::from_secs(5))?;
    let files = list(&mut p, name)?;
    if files.is_empty() {
        bail!("no {name}/ on the card (console: ls)");
    }
    if !files.iter().any(|f| f == "MANIFEST.SIG") {
        bail!("{name} is not sealed yet (console: bundle seal)");
    }
    let mut got = Vec::new();
    for f in &files {
        let Some(c) = canonical(f) else {
            eprintln!("  skipping {f}: not a bundle member");
            continue;
        };
        let bytes = fetch(&mut p, &format!("{name}/{f}"))?;
        println!("  {f:<13} -> {c:<17} {:>7} bytes  crc ok", bytes.len());
        got.push((c, bytes));
    }
    let manifest = got
        .iter()
        .find(|(n, _)| n == "manifest.cbor")
        .map(|(_, b)| b.clone())
        .context("no manifest")?;
    let m = cairn_format::Manifest::from_cbor(&manifest)
        .map_err(|e| anyhow::anyhow!("manifest: {e}"))?;
    let dir = out.join(cairn_format::hex(&m.bundle_id));
    std::fs::create_dir_all(&dir)?;
    for (n, b) in &got {
        std::fs::write(dir.join(n), b)?;
    }
    println!(
        "{name} -> {}\n  device_counter {}, {} members, frames seq {}..{}, utc_basis_ms {} (±{} ms), {:?}",
        dir.display(),
        m.device_counter,
        m.members.len(),
        m.first_seq,
        m.last_seq,
        m.utc_basis_ms,
        m.utc_basis_acc_ms,
        m.recovery_state
    );
    for (rt, n) in &m.record_counts {
        println!("  {:<17} {n}", rt.name());
    }
    Ok(())
}

fn device_key(port: Option<&str>) -> Result<String> {
    let mut p = Port::open(port, Duration::from_secs(5))?;
    let l = p.ask(
        "bundle",
        "bundle:",
        &["device_key="],
        Duration::from_secs(4),
    )?;
    l.split("device_key=")
        .nth(1)
        .map(|k| k.trim().to_string())
        .context("no device_key in the reply")
}

fn verify(dir: &Path, key: Option<String>, server: &Path, port: Option<&str>) -> Result<bool> {
    let key = match key {
        Some(k) => k,
        None => device_key(port)?,
    };
    let dir = std::fs::canonicalize(dir)?;
    println!("cairn-verify -device-key {key} {}", dir.display());
    let status = std::process::Command::new("go")
        .args(["run", "./cmd/cairn-verify", "-v", "-device-key", &key])
        .arg(&dir)
        .current_dir(server)
        .status()
        .with_context(|| format!("go run in {}", server.display()))?;
    Ok(status.success())
}
