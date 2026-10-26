//! Check a session pulled off the card: every file well formed, rows in time
//! order, each stream at its expected rate, values physically plausible.
//! Stationary-bench expectations (the IMU reads ~1 g) only with --stationary.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(clap::Args)]
pub struct Args {
    /// Directory of pulled files (GNSS.CSV, IMU.CSV, ... possibly prefixed).
    dir: PathBuf,
    #[arg(long)]
    stationary: bool,
}

/// (file, expected rows per second, tolerance as a fraction)
const RATES: [(&str, f64, f64); 6] = [
    ("IMU.CSV", 50.0, 0.15),
    ("BARO.CSV", 4.0, 0.15),
    ("MAG.CSV", 10.0, 0.25),
    ("GNSS.CSV", 1.0, 0.10),
    ("POWER.CSV", 0.2, 0.25),
    ("PPS.CSV", 1.0, 0.10),
];

struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    col: HashMap<String, usize>,
}

impl Table {
    fn load(path: &Path) -> Result<Table> {
        let text = std::fs::read_to_string(path)?;
        let mut lines = text.lines().filter(|l| !l.is_empty());
        let header: Vec<String> = lines
            .next()
            .unwrap_or("")
            .split(',')
            .map(String::from)
            .collect();
        let rows = lines.map(split_csv).collect();
        let col = header
            .iter()
            .enumerate()
            .map(|(i, h)| (h.clone(), i))
            .collect();
        Ok(Table { header, rows, col })
    }
    fn f(&self, r: &[String], name: &str) -> f64 {
        self.col
            .get(name)
            .and_then(|&i| r.get(i))
            .and_then(|v| v.parse().ok())
            .unwrap_or(f64::NAN)
    }
    fn s<'a>(&self, r: &'a [String], name: &str) -> &'a str {
        self.col
            .get(name)
            .and_then(|&i| r.get(i))
            .map_or("", |v| v.as_str())
    }
}

/// CSV fields, honouring double quotes (EVENTS.CSV details contain commas).
fn split_csv(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out
}

struct Report {
    failures: usize,
}

impl Report {
    fn check(&mut self, ok: bool, msg: impl AsRef<str>) {
        println!("  {}  {}", if ok { "ok  " } else { "FAIL" }, msg.as_ref());
        if !ok {
            self.failures += 1;
        }
    }
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|f| f.to_string_lossy().to_uppercase().ends_with(name))
        })
}

pub fn run(a: Args) -> Result<bool> {
    let mut rep = Report { failures: 0 };
    for name in [
        "GNSS.CSV",
        "SAT.CSV",
        "IMU.CSV",
        "BARO.CSV",
        "MAG.CSV",
        "POWER.CSV",
        "EVENTS.CSV",
        "PPS.CSV",
    ] {
        println!("\n{name}");
        let Some(path) = find(&a.dir, name) else {
            rep.check(false, format!("{name} present"));
            continue;
        };
        let t = Table::load(&path)?;
        rep.check(
            t.header.first().is_some_and(|h| h == "t_ms"),
            format!("header starts with t_ms ({} columns)", t.header.len()),
        );
        if name == "PPS.CSV" && t.rows.is_empty() {
            // The receiver emits a time pulse only once it has a fix.
            let had_fix = find(&a.dir, "GNSS.CSV")
                .and_then(|p| Table::load(&p).ok())
                .is_some_and(|g| g.rows.iter().any(|r| g.s(r, "fix_ok") == "1"));
            rep.check(!had_fix, "no PPS rows, and no GNSS fix to cause any");
            continue;
        }
        rep.check(!t.rows.is_empty(), format!("{} rows", t.rows.len()));
        if t.rows.is_empty() {
            continue;
        }
        let bad = t.rows.iter().position(|r| r.len() != t.header.len());
        rep.check(
            bad.is_none(),
            format!(
                "every row has {} columns{}",
                t.header.len(),
                bad.map_or(String::new(), |i| format!(" (first bad row {})", i + 2))
            ),
        );
        let ts: Vec<i64> = t.rows.iter().filter_map(|r| r[0].parse().ok()).collect();
        let back = ts.windows(2).filter(|w| w[1] < w[0]).count();
        rep.check(
            back == 0,
            format!("t_ms never goes backwards ({back} steps back)"),
        );
        let span = if ts.len() > 1 {
            (ts[ts.len() - 1] - ts[0]) as f64 / 1000.0
        } else {
            0.0
        };
        if let Some(&(_, want, tol)) = RATES.iter().find(|r| r.0 == name)
            && span > 10.0
        {
            let got = (ts.len() - 1) as f64 / span;
            rep.check(
                (got - want).abs() <= want * tol,
                format!(
                    "rate {got:.2}/s over {span:.0} s (expect {want}/s ±{:.0}%)",
                    tol * 100.0
                ),
            );
            println!(
                "        largest gap {} ms",
                ts.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0)
            );
        }
        let rows = &t.rows;
        match name {
            "IMU.CSV" => {
                let mags: Vec<f64> = rows
                    .iter()
                    .map(|r| {
                        ["ax_mg", "ay_mg", "az_mg"]
                            .iter()
                            .map(|k| t.f(r, k).powi(2))
                            .sum::<f64>()
                            .sqrt()
                    })
                    .collect();
                let mean = mags.iter().sum::<f64>() / mags.len() as f64;
                let (lo, hi) = mags
                    .iter()
                    .fold((f64::MAX, f64::MIN), |(a, b), &m| (a.min(m), b.max(m)));
                println!("        |a| mean {mean:.0} mg, min {lo:.0}, max {hi:.0}");
                if a.stationary {
                    rep.check(
                        900.0 < mean && mean < 1100.0,
                        "stationary: |a| averages ~1 g",
                    );
                    rep.check(
                        hi < 1500.0,
                        "stationary: no sample above 1.5 g (filters settled before recording)",
                    );
                }
                rep.check(
                    rows.iter()
                        .all(|r| (-20.0..70.0).contains(&t.f(r, "temp_c"))),
                    "IMU temperature in -20..70 C",
                );
                let mut gyr: Vec<f64> = rows
                    .iter()
                    .flat_map(|r| ["gx_dps", "gy_dps", "gz_dps"].map(|k| t.f(r, k).abs()))
                    .collect();
                gyr.sort_by(f64::total_cmp);
                if a.stationary {
                    rep.check(
                        gyr[gyr.len() / 2] < 5.0,
                        "stationary: median |gyro| < 5 dps",
                    );
                }
            }
            "BARO.CSV" => {
                let p: Vec<f64> = rows.iter().map(|r| t.f(r, "pressure_pa")).collect();
                let (lo, hi) = p
                    .iter()
                    .fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
                rep.check(
                    lo > 30_000.0 && hi < 110_000.0,
                    format!(
                        "pressure in 300..1100 hPa ({:.1}..{:.1})",
                        lo / 100.0,
                        hi / 100.0
                    ),
                );
                let e: Vec<f64> = rows.iter().map(|r| t.f(r, "elevation_m")).collect();
                let (elo, ehi) = e
                    .iter()
                    .fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
                let last = rows.last().unwrap();
                println!(
                    "        elevation {elo:.1}..{ehi:.1} m, gain {} m, loss {} m",
                    t.s(last, "gain_m"),
                    t.s(last, "loss_m")
                );
                if a.stationary {
                    rep.check(ehi - elo < 3.0, "stationary: elevation varies < 3 m");
                }
                rep.check(
                    rows.iter()
                        .all(|r| (0.0..=100.0).contains(&t.f(r, "humidity_pct"))),
                    "humidity in 0..100 %",
                );
            }
            "GNSS.CSV" => {
                let fixes: Vec<_> = rows.iter().filter(|r| t.s(r, "fix_ok") == "1").collect();
                let max_sv = rows
                    .iter()
                    .map(|r| t.f(r, "num_sv") as i64)
                    .max()
                    .unwrap_or(0);
                println!(
                    "        {} of {} epochs with a fix; max sats {max_sv}",
                    fixes.len(),
                    rows.len()
                );
                let bad = fixes.iter().find(|r| {
                    !((-90.0..=90.0).contains(&t.f(r, "lat"))
                        && (-180.0..=180.0).contains(&t.f(r, "lon")))
                });
                rep.check(bad.is_none(), "every fix has coordinates in range");
            }
            "SAT.CSV" => {
                let best = rows
                    .iter()
                    .map(|r| t.f(r, "cno_dbhz") as i64)
                    .max()
                    .unwrap_or(0);
                println!(
                    "        {} satellite rows, best C/N0 {best} dB-Hz",
                    rows.len()
                );
            }
            "MAG.CSV" => rep.check(
                rows.iter().any(|r| t.f(r, "mx") != 0.0),
                "magnetometer reads non-zero",
            ),
            "POWER.CSV" => {
                let v: Vec<i64> = rows.iter().map(|r| t.f(r, "vbus_mv") as i64).collect();
                println!(
                    "        vbus {}..{} mV, battery={}",
                    v.iter().min().unwrap(),
                    v.iter().max().unwrap(),
                    t.s(rows.last().unwrap(), "battery")
                );
                let lora_off = rows.iter().all(|r| {
                    u8::from_str_radix(t.s(r, "ldo").trim_start_matches("0x"), 16)
                        .is_ok_and(|l| l & 0x04 == 0)
                });
                rep.check(lora_off, "ALDO3 (LoRa) off in every power row");
            }
            "EVENTS.CSV" => {
                let names: BTreeSet<&str> = rows
                    .iter()
                    .filter_map(|r| r.get(1).map(String::as_str))
                    .collect();
                println!(
                    "        events: {}",
                    names.iter().copied().collect::<Vec<_>>().join(", ")
                );
                for must in ["boot", "session", "pmu", "rails", "imu", "gnss_version"] {
                    rep.check(names.contains(must), format!("event '{must}' recorded"));
                }
                for r in rows.iter().filter(|r| {
                    matches!(
                        r.get(1).map(String::as_str),
                        Some("fix_acquired" | "prime" | "prime_plan" | "assist_sent")
                    )
                }) {
                    println!("        {}: {}", r[1], r.get(2).map_or("", |s| s.as_str()));
                }
            }
            _ => {}
        }
    }
    println!(
        "\n{}",
        if rep.failures > 0 {
            format!("FAILED: {}", rep.failures)
        } else {
            "all checks passed".into()
        }
    );
    Ok(rep.failures == 0)
}
