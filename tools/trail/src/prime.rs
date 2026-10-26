//! Prime the board's GNSS receiver from this computer: its clock (always) and
//! optionally a position. The board sends both to the receiver at once, sets
//! its RTC, and records them in PRIME.TXT so the next cold power-on starts
//! from them too.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

use crate::serial::Port;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    port: Option<String>,
    /// Latitude, decimal degrees (WGS84).
    #[arg(long, allow_hyphen_values = true, requires = "lon")]
    lat: Option<String>,
    /// Longitude, decimal degrees (WGS84).
    #[arg(long, allow_hyphen_values = true, requires = "lat")]
    lon: Option<String>,
    /// Height above the WGS84 ellipsoid in metres (not above sea level; about
    /// 30 m lower than sea level height on the US West Coast). 0 if unknown.
    #[arg(long, allow_hyphen_values = true, default_value = "0")]
    alt: String,
    /// How sure the position is, in metres.
    #[arg(long, default_value = "100")]
    acc: String,
    /// Show the board's PRIME.TXT and plan without sending anything.
    #[arg(long)]
    show: bool,
}

fn check(name: &str, v: &str, digits: u32, range: std::ops::RangeInclusive<i64>) -> Result<()> {
    match trail_core::prime::parse_fixed(v, digits) {
        Some(x) if range.contains(&x) => Ok(()),
        _ => bail!("--{name} {v:?} is not a number in range"),
    }
}

pub fn run(a: Args) -> Result<()> {
    let mut p = Port::open(a.port.as_deref(), Duration::from_secs(5))?;
    if a.show {
        return show(&mut p);
    }
    if let (Some(lat), Some(lon)) = (&a.lat, &a.lon) {
        check("lat", lat, 7, -900_000_000..=900_000_000)?;
        check("lon", lon, 7, -1_800_000_000..=1_800_000_000)?;
        check("alt", &a.alt, 2, -100_000_000..=100_000_000)?;
        check("acc", &a.acc, 2, 1..=4_000_000_000)?;
    }
    // The clock last, just before sending, so the stamp is fresh.
    let ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    println!(
        "{}",
        p.ask(
            &format!("prime time {ms}"),
            "prime:",
            &["time", "implausible"],
            Duration::from_secs(3)
        )?
    );
    if let (Some(lat), Some(lon)) = (&a.lat, &a.lon) {
        let cmd = format!("prime pos {lat} {lon} {} {}", a.alt, a.acc);
        println!(
            "{}",
            p.ask(
                &cmd,
                "prime:",
                &["position", "out of range", "pos <"],
                Duration::from_secs(3)
            )?
        );
    }
    show(&mut p)
}

fn show(p: &mut Port) -> Result<()> {
    p.drain()?;
    p.send("prime show")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while let Some(l) = p.line(deadline)? {
        if l.starts_with("DEBUG") || l.starts_with("INFO") || l.starts_with("WARN") {
            continue;
        }
        let stop = l.starts_with("prime: state=") || l.starts_with("prime: card");
        if stop
            || l.starts_with("prime")
            || l.starts_with('#')
            || l.contains('=')
            || l.starts_with("(no ")
        {
            println!("  {l}");
        }
        if stop {
            break;
        }
    }
    Ok(())
}
