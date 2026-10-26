//! The board's USB serial console (the ESP32-S3's USB-Serial-JTAG).

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

pub struct Port {
    name: String,
    port: Box<dyn serialport::SerialPort>,
    /// Bytes received but not yet consumed as lines.
    pending: Vec<u8>,
}

/// The first `/dev/cu.usbmodem*`, unless one is named.
pub fn find(explicit: Option<&str>) -> Result<String> {
    if let Some(p) = explicit {
        return Ok(p.to_string());
    }
    let mut ports: Vec<String> = std::fs::read_dir("/dev")?
        .filter_map(|e| e.ok())
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.starts_with("/dev/cu.usbmodem"))
        .collect();
    ports.sort();
    ports
        .into_iter()
        .next()
        .context("no /dev/cu.usbmodem*: is the board plugged in?")
}

impl Port {
    /// Open the console, retrying for `wait` while the board re-enumerates.
    pub fn open(explicit: Option<&str>, wait: Duration) -> Result<Self> {
        let deadline = Instant::now() + wait;
        loop {
            let attempt = find(explicit).and_then(|name| {
                let port = serialport::new(&name, 115_200)
                    .timeout(Duration::from_millis(50))
                    .open()
                    .with_context(|| format!("open {name}"))?;
                Ok(Port {
                    name,
                    port,
                    pending: Vec::new(),
                })
            });
            match attempt {
                Ok(mut p) => {
                    // End whatever partial line the console holds (esptool's
                    // sync bytes, an interrupted command) so the first command
                    // arrives on a clean line.
                    let _ = p.port.write_all(b"\n");
                    return Ok(p);
                }
                Err(e) if Instant::now() > deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Reset into the application through the USB-Serial-JTAG's DTR/RTS lines.
    pub fn reset(&mut self) -> Result<()> {
        self.port.write_data_terminal_ready(false)?;
        self.port.write_request_to_send(true)?;
        std::thread::sleep(Duration::from_millis(100));
        self.port.write_request_to_send(false)?;
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    /// Whatever has arrived, waiting at most the port timeout. A read error
    /// (the board re-enumerating) is returned so the caller can reopen.
    pub fn read_some(&mut self) -> Result<Vec<u8>> {
        let mut buf = [0u8; 4096];
        match self.port.read(&mut buf) {
            Ok(n) => Ok(buf[..n].to_vec()),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn send(&mut self, line: &str) -> Result<()> {
        self.port.write_all(line.as_bytes())?;
        self.port.write_all(b"\n")?;
        self.port.flush()?;
        Ok(())
    }

    /// Drop everything received so far.
    pub fn drain(&mut self) -> Result<()> {
        self.pending.clear();
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            if self.read_some()?.is_empty() {
                break;
            }
        }
        Ok(())
    }

    /// The next complete line, or None once `deadline` passes.
    pub fn line(&mut self, deadline: Instant) -> Result<Option<String>> {
        loop {
            if let Some(i) = self.pending.iter().position(|&b| b == b'\n') {
                let raw: Vec<u8> = self.pending.drain(..=i).collect();
                return Ok(Some(
                    String::from_utf8_lossy(&raw)
                        .trim_end_matches(['\r', '\n'])
                        .to_string(),
                ));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            let chunk = self.read_some()?;
            self.pending.extend_from_slice(&chunk);
        }
    }

    /// Send a command and return the first reply line that starts with
    /// `prefix` and contains one of `expect` (any, if empty). Log lines from
    /// the firmware in between are skipped.
    pub fn ask(
        &mut self,
        cmd: &str,
        prefix: &str,
        expect: &[&str],
        wait: Duration,
    ) -> Result<String> {
        self.drain()?;
        self.send(cmd)?;
        let deadline = Instant::now() + wait;
        let trace = std::env::var_os("TRAIL_TRACE").is_some();
        while let Some(l) = self.line(deadline)? {
            if trace {
                eprintln!("  < {l}");
            }
            if l.starts_with(prefix) && (expect.is_empty() || expect.iter().any(|e| l.contains(e)))
            {
                return Ok(l);
            }
        }
        let shown: String = cmd.chars().take(40).collect();
        bail!("no `{prefix}` reply to `{shown}` within {wait:?}")
    }

    /// Send a payload through one of the firmware's chunked commands (`prov`,
    /// `assist`): `<cmd> begin`, then `<cmd> data <hex>` lines, each answered
    /// with the running byte count before the next is sent, then `<cmd> commit
    /// <crc32>`. Each data line fits the USB-Serial-JTAG's 64-byte receive
    /// FIFO whole; longer lines lost bytes. Returns the commit reply.
    pub fn upload(&mut self, cmd: &str, payload: &[u8], progress: bool) -> Result<String> {
        let tag = format!("{cmd}:");
        self.ask(
            &format!("{cmd} begin"),
            &tag,
            &["ready"],
            Duration::from_secs(3),
        )?;
        let mut sent = 0usize;
        for chunk in payload.chunks(24) {
            sent += chunk.len();
            let r = self.ask(
                &format!("{cmd} data {}", hex::encode(chunk)),
                &tag,
                &["ok", "BAD", "FULL"],
                Duration::from_secs(3),
            )?;
            if r != format!("{cmd}: ok {sent}") {
                bail!("device has {r:?}, expected {sent} bytes");
            }
            if progress && sent % 2400 < 24 {
                eprint!("\r  {sent} / {} bytes", payload.len());
            }
        }
        if progress {
            eprintln!("\r  {sent} / {} bytes", payload.len());
        }
        let crc = crc32fast::hash(payload);
        self.ask(
            &format!("{cmd} commit {crc:08x}"),
            &tag,
            &["written", "MISMATCH", "FAILED"],
            Duration::from_secs(15),
        )
    }
}
