//! Capture the console, send commands, and pull files off the card with
//! `cat`, checking each against the CRC-32 the firmware prints after it.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::serial::Port;

pub const SESSION_FILES: [&str; 9] = [
    "INFO.TXT",
    "EVENTS.CSV",
    "GNSS.CSV",
    "SAT.CSV",
    "BARO.CSV",
    "MAG.CSV",
    "POWER.CSV",
    "PPS.CSV",
    "IMU.CSV",
];

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    port: Option<String>,
    /// Reset the chip before capturing.
    #[arg(long)]
    reset: bool,
    /// Capture time after the last command, in seconds.
    #[arg(long, default_value_t = 10.0)]
    seconds: f64,
    /// Seconds to wait before sending commands.
    #[arg(long, default_value_t = 0.0)]
    wait: f64,
    /// A console command (repeatable).
    #[arg(long = "cmd")]
    cmds: Vec<String>,
    /// A file to `cat` and save (repeatable), e.g. S0009/GNSS.CSV.
    #[arg(long = "pull")]
    pulls: Vec<String>,
    /// Pull every file of this session, e.g. S0009.
    #[arg(long)]
    session: Option<String>,
    /// Directory for pulled files.
    #[arg(long, default_value = ".")]
    out: PathBuf,
    /// Append everything received to this file.
    #[arg(long)]
    log: Option<PathBuf>,
    #[arg(long)]
    quiet: bool,
}

const END: &[u8] = b"----- END ";

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

pub fn run(mut a: Args) -> Result<bool> {
    if let Some(s) = &a.session {
        a.pulls
            .extend(SESSION_FILES.iter().map(|f| format!("{s}/{f}")));
    }
    let mut port = Port::open(a.port.as_deref(), Duration::from_secs(10))?;
    if a.reset {
        port.reset()?;
        drop(port);
        std::thread::sleep(Duration::from_millis(300));
        port = Port::open(a.port.as_deref(), Duration::from_secs(10))?;
    }
    let mut log = match &a.log {
        Some(p) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)?,
        ),
        None => None,
    };

    let mut buf: Vec<u8> = Vec::new();
    let mut pending: std::collections::VecDeque<String> = a
        .cmds
        .iter()
        .cloned()
        .chain(a.pulls.iter().map(|p| format!("cat {p}")))
        .collect();
    let start = Instant::now();
    let mut last_cmd = start;
    let mut next_cmd_at = start + Duration::from_secs_f64(a.wait);
    let mut end_at: Option<Instant> = None;
    // While a `cat` streams: the END count to exceed, and a deadline.
    let mut waiting: Option<(usize, Instant)> = None;

    loop {
        let now = Instant::now();
        if let Some((ends, deadline)) = waiting
            && (count(&buf, END) > ends || now > deadline)
        {
            waiting = None;
            next_cmd_at = now + Duration::from_millis(300);
        }
        if waiting.is_none()
            && now >= next_cmd_at
            && let Some(cmd) = pending.pop_front()
        {
            port.send(&cmd)?;
            last_cmd = now;
            if cmd.starts_with("cat") {
                waiting = Some((count(&buf, END), now + Duration::from_secs(120)));
            } else {
                next_cmd_at = now + Duration::from_millis(700);
            }
        }
        if pending.is_empty() && waiting.is_none() && end_at.is_none() && now >= next_cmd_at {
            end_at = Some(last_cmd.max(start) + Duration::from_secs_f64(a.seconds));
        }
        if end_at.is_some_and(|t| now >= t) {
            break;
        }
        let chunk = match port.read_some() {
            Ok(c) => c,
            Err(_) => {
                drop(port);
                port = Port::open(a.port.as_deref(), Duration::from_secs(10))?;
                continue;
            }
        };
        if !chunk.is_empty() {
            buf.extend_from_slice(&chunk);
            if let Some(l) = log.as_mut() {
                l.write_all(&chunk)?;
            }
            if !a.quiet {
                print!("{}", String::from_utf8_lossy(&chunk));
                std::io::stdout().flush()?;
            }
        }
    }
    save_pulls(&buf, &a.out)
}

/// Every `----- BEGIN name n bytes -----` … `----- END name n bytes
/// crc32=xxxxxxxx -----` block, saved and checked. Returns whether all matched.
fn save_pulls(text: &[u8], out: &std::path::Path) -> Result<bool> {
    let mut ok_all = true;
    let mut rest = text;
    while let Some(i) = find(rest, b"----- BEGIN ") {
        rest = &rest[i..];
        let Some(nl) = rest.iter().position(|&b| b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&rest[..nl]).to_string();
        let mut words = header.split_whitespace().skip(2);
        let (Some(name), Some(declared)) = (words.next(), words.next()) else {
            break;
        };
        let body_start = nl + 1;
        let Some(e) = find(&rest[body_start..], b"\n----- END ") else {
            break;
        };
        let mut body = &rest[body_start..body_start + e];
        if body.last() == Some(&b'\r') {
            body = &body[..body.len() - 1];
        }
        let tail_start = body_start + e + 1;
        let tail_end = rest[tail_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(rest.len(), |p| tail_start + p);
        let tail = String::from_utf8_lossy(&rest[tail_start..tail_end]).to_string();
        let t: Vec<&str> = tail.split_whitespace().collect();
        // ----- END <name> <total> bytes crc32=<hex> -----
        let total: usize = t.get(3).and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
        let crc = t
            .get(5)
            .and_then(|s| s.strip_prefix("crc32="))
            .unwrap_or("");
        let got = format!("{:08x}", crc32fast::hash(body));
        let ok = got == crc && body.len() == total;
        ok_all &= ok;
        std::fs::create_dir_all(out)?;
        let dest = out.join(name.replace('/', "_"));
        std::fs::write(&dest, body)?;
        println!(
            "\n[pull] {name}: {} bytes (declared {declared}, streamed {total}) crc32 {got} vs {crc} -> {} -> {}",
            body.len(),
            if ok { "OK" } else { "MISMATCH" },
            dest.display()
        );
        rest = &rest[tail_end..];
    }
    Ok(ok_all)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
