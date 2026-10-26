//! Wi-Fi, TCP/IP and TLS to the Cairn server.
//!
//! Stage 1 of the uplink: join the provisioned Wi-Fi network, take an address
//! by DHCP, and check in with the Cairn server's device listener over mutual
//! TLS — the same listener the dongle uploads to, which identifies a device by
//! its client certificate. The check-in is `GET /api/v2/health`; a 200 proves
//! Wi-Fi, TLS, the server's certificate against the pinned Cairn CA, and this
//! device's certificate, end to end. Bundle upload builds on this connection.
//!
//! Everything it needs comes from the `keys` store, provisioned over USB; if
//! the store is empty the radio is never started.

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::net::Ipv4Addr;

use embassy_executor::Spawner;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Config as NetConfig, IpAddress, Runner, Stack, StackResources};
use embassy_time::{Duration, Timer, with_timeout};
use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, SignatureScheme, TlsClock, TlsConfig,
    TlsConnection, TlsContext,
};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config as WifiConfig, Interface, WifiController,
};
use static_cell::StaticCell;

use crate::keys::{self, tag};
use crate::shared::{event, with_status};

/// How often to check in while the network is up.
const CHECKIN_EVERY: Duration = Duration::from_secs(600);

/// TLS certificate dates are checked against the GNSS time once there is one,
/// and otherwise against the firmware's build time — a floor nothing valid can
/// precede. Never against the unanchored RTC.
pub struct Clock;
impl TlsClock for Clock {
    fn now() -> Option<u64> {
        let gnss = crate::shared::status()
            .utc
            .filter(|t| trail_core::pcf8563::plausible(t, false))
            .map(|t| trail_core::pcf8563::unix_seconds(&t) as u64);
        Some(gnss.unwrap_or(crate::shared::BUILD_EPOCH))
    }
}

/// The TLS provider: the hardware TRNG, the pinned Cairn CA, and this
/// device's P-256 client certificate and key.
struct Provider<'a> {
    rng: esp_hal::rng::Trng,
    verifier: CertVerifier<'a, Aes128GcmSha256, Clock, 2048>,
    cert: &'a [u8],
    key: &'a [u8],
}

impl CryptoProvider for Provider<'_> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = p256::ecdsa::DerSignature;

    fn rng(&mut self) -> impl embedded_tls::CryptoRngCore {
        &mut self.rng
    }

    fn verifier(
        &mut self,
    ) -> Result<&mut impl embedded_tls::TlsVerifier<Self::CipherSuite>, embedded_tls::TlsError>
    {
        Ok(&mut self.verifier)
    }

    fn signer(
        &mut self,
    ) -> Result<(impl signature::SignerMut<Self::Signature>, SignatureScheme), embedded_tls::TlsError>
    {
        let secret = p256::SecretKey::from_sec1_der(self.key)
            .map_err(|_| embedded_tls::TlsError::InvalidPrivateKey)?;
        Ok((
            p256::ecdsa::SigningKey::from(&secret),
            SignatureScheme::EcdsaSecp256r1Sha256,
        ))
    }

    fn client_cert(&mut self) -> Option<Certificate<impl AsRef<[u8]>>> {
        Some(Certificate::X509(self.cert))
    }
}

/// What the network needs from the keys store, or why it cannot start.
pub struct Settings {
    ssid: String,
    password: String,
    bssid: Option<[u8; 6]>,
    channel: Option<u8>,
    host: String,
    ip: Option<Ipv4Addr>,
    port: u16,
    cert: Vec<u8>,
    key: Vec<u8>,
    ca: Vec<u8>,
}

impl Settings {
    pub fn load() -> Result<Self, &'static str> {
        let need = |t, what| keys::get(t).ok_or(what);
        let bssid = keys::get(tag::WIFI_BSSID).and_then(|v| <[u8; 6]>::try_from(v.as_slice()).ok());
        Ok(Self {
            ssid: keys::get_str(tag::WIFI_SSID).ok_or("no Wi-Fi SSID")?,
            password: keys::get_str(tag::WIFI_PASSWORD).unwrap_or_default(),
            bssid,
            channel: keys::get(tag::WIFI_CHANNEL).and_then(|v| v.first().copied()),
            host: keys::get_str(tag::SERVER_HOST).ok_or("no server host")?,
            ip: keys::get(tag::SERVER_IP)
                .and_then(|v| <[u8; 4]>::try_from(v.as_slice()).ok())
                .map(Ipv4Addr::from),
            port: keys::get(tag::SERVER_PORT)
                .filter(|v| v.len() == 2)
                .map(|v| u16::from_le_bytes([v[0], v[1]]))
                .unwrap_or(8443),
            cert: need(tag::CLIENT_CERT, "no client certificate")?,
            key: need(tag::CLIENT_KEY, "no client key")?,
            ca: need(tag::CA_CERT, "no CA certificate")?,
        })
    }
}

static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
static SETTINGS: StaticCell<Settings> = StaticCell::new();

/// Start Wi-Fi and the uplink if the device is provisioned.
pub fn start(spawner: &Spawner, wifi: esp_hal::peripherals::WIFI<'static>) {
    let settings = match Settings::load() {
        Ok(s) => s,
        Err(why) => {
            event(
                "wifi",
                format_args!("off: {} (provision with scripts/provision.py)", why),
            );
            return;
        }
    };
    let controller = match WifiController::new(wifi, Default::default()) {
        Ok(c) => c,
        Err(e) => {
            event("wifi", format_args!("radio init failed: {:?}", e));
            return;
        }
    };
    let interface = Interface::station();
    let seed = {
        let r = esp_hal::rng::Rng::new();
        (r.random() as u64) << 32 | r.random() as u64
    };
    let (stack, runner) = embassy_net::new(
        interface,
        NetConfig::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    let settings: &'static Settings = SETTINGS.init(settings);
    spawner.spawn(wifi_task(controller, settings).unwrap());
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(uplink_task(stack, settings).unwrap());
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}

/// Join the network and stay joined: reconnect after a drop, backing off.
#[embassy_executor::task]
async fn wifi_task(mut controller: WifiController<'static>, s: &'static Settings) {
    let (Ok(ssid), Ok(password)) = (s.ssid.as_str().try_into(), s.password.as_str().try_into())
    else {
        event("wifi", format_args!("SSID or password too long"));
        return;
    };
    let mut station = StationConfig::default()
        .with_ssid(ssid)
        .with_authentication(AuthenticationMethodConfig::Wpa2Personal(password));
    if let Some(b) = s.bssid {
        station = station.with_bssid(b);
    }
    if let Some(c) = s.channel {
        station = station.with_channel(c);
    }
    if let Err(e) = controller.set_config(&WifiConfig::Station(station)) {
        event("wifi", format_args!("config refused: {:?}", e));
        return;
    }
    let mut backoff = 5u64;
    loop {
        match controller.connect_async().await {
            Ok(info) => {
                backoff = 5;
                let rssi = controller.rssi().unwrap_or(0);
                event(
                    "wifi",
                    format_args!(
                        "joined {} bssid {:02x?} channel {} rssi {} dBm",
                        s.ssid, info.bssid, info.channel, rssi
                    ),
                );
                with_status(|st| st.wifi_up = true);
                let why = controller.wait_for_disconnect_async().await;
                with_status(|st| st.wifi_up = false);
                event(
                    "wifi",
                    format_args!("disconnected: {:?}", why.map(|d| d.reason)),
                );
            }
            Err(e) => {
                event(
                    "wifi",
                    format_args!("join failed: {:?}; retry in {} s", e, backoff),
                );
                Timer::after(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(300);
            }
        }
    }
}

/// Check in now and then while the network is up.
#[embassy_executor::task]
async fn uplink_task(stack: Stack<'static>, s: &'static Settings) {
    loop {
        stack.wait_config_up().await;
        if let Some(cfg) = stack.config_v4() {
            event(
                "net",
                format_args!("address {} gateway {:?}", cfg.address, cfg.gateway),
            );
        }
        match checkin(stack, s).await {
            Ok((code, body)) => {
                with_status(|st| st.checkins += 1);
                event("checkin", format_args!("HTTP {} {}", code, body.trim()));
            }
            Err(e) => {
                with_status(|st| st.checkin_errors += 1);
                event("checkin", format_args!("failed: {}", e));
            }
        }
        Timer::after(CHECKIN_EVERY).await;
    }
}

async fn resolve(stack: Stack<'static>, s: &Settings) -> Result<IpAddress, String> {
    if let Some(ip) = s.ip {
        return Ok(IpAddress::Ipv4(ip));
    }
    let addrs = stack
        .dns_query(&s.host, embassy_net::dns::DnsQueryType::A)
        .await
        .map_err(|e| alloc::format!("DNS {}: {:?}", s.host, e))?;
    addrs
        .first()
        .copied()
        .ok_or_else(|| alloc::format!("DNS {}: no address", s.host))
}

/// One request over a fresh mutual-TLS connection. Returns the status code
/// and the start of the body.
async fn checkin(stack: Stack<'static>, s: &Settings) -> Result<(u16, String), String> {
    let ip = resolve(stack, s).await?;
    let mut rx = Box::new([0u8; 4096]);
    let mut tx = Box::new([0u8; 4096]);
    let mut socket = TcpSocket::new(stack, &mut rx[..], &mut tx[..]);
    socket.set_timeout(Some(Duration::from_secs(20)));
    with_timeout(Duration::from_secs(15), socket.connect((ip, s.port)))
        .await
        .map_err(|_| alloc::format!("TCP {}:{} timed out", ip, s.port))?
        .map_err(|e| alloc::format!("TCP {}:{}: {:?}", ip, s.port, e))?;

    let mut read_buf = alloc::vec![0u8; 16640];
    let mut write_buf = alloc::vec![0u8; 8192];
    let config = TlsConfig::new().with_server_name(&s.host);
    let mut tls = TlsConnection::new(socket, &mut read_buf, &mut write_buf);
    let rng = esp_hal::rng::Trng::try_new().map_err(|_| String::from("no TRNG"))?;
    let provider = Provider {
        rng,
        verifier: CertVerifier::new(Certificate::X509(&s.ca[..])),
        cert: &s.cert,
        key: &s.key,
    };
    with_timeout(
        Duration::from_secs(30),
        tls.open(TlsContext::new(&config, provider)),
    )
    .await
    .map_err(|_| String::from("TLS handshake timed out"))?
    .map_err(|e| alloc::format!("TLS handshake: {:?}", e))?;

    let mut req = String::new();
    let _ = write!(
        req,
        "GET /api/v2/health HTTP/1.1\r\nHost: {}\r\nUser-Agent: cairn-trail/{}\r\nConnection: close\r\n\r\n",
        s.host,
        crate::shared::VERSION
    );
    tls.write(req.as_bytes())
        .await
        .map_err(|e| alloc::format!("TLS write: {:?}", e))?;
    tls.flush()
        .await
        .map_err(|e| alloc::format!("TLS flush: {:?}", e))?;

    let mut resp = Vec::new();
    let mut buf = [0u8; 512];
    while resp.len() < 2048 {
        match with_timeout(Duration::from_secs(15), tls.read(&mut buf)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => resp.extend_from_slice(&buf[..n]),
            Ok(Err(_)) => break, // close_notify or reset after the body
        }
        if let Some(h) = find(&resp, b"\r\n\r\n")
            && (resp.len() >= h + 4 + 64 || resp.windows(5).any(|w| w == b"\r\n0\r\n"))
        {
            break;
        }
    }
    let _ = tls.close().await;
    let text = String::from_utf8_lossy(&resp);
    let code = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| alloc::format!("no HTTP status in {:?}", &text[..text.len().min(80)]))?;
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .chars()
        .take(120)
        .collect();
    Ok((code, body))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
