//! Cairn trail device firmware, v0.1: a capture logger for the LILYGO T-Beam
//! Supreme. GNSS, IMU, barometer and elevation, magnetometer, power and PPS,
//! each to its own CSV file on the SD card. The LoRa radio stays off.
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

mod bundle;
mod display;
mod gnss;
mod ident;
mod imu;
mod keys;
mod net;
mod pmu;
mod sensors;
mod shared;
mod storage;
mod tasks;
mod vectors;

use embassy_executor::Spawner;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::interrupt::Priority;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtag;
use esp_rtos::embassy::InterruptExecutor;
use heapless::String;
use static_cell::StaticCell;

use crate::shared::event;

esp_bootloader_esp_idf::esp_app_desc!();

#[cfg(feature = "verbose")]
const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Debug;
#[cfg(not(feature = "verbose"))]
const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Info;

/// Our own modules log at `LOG_LEVEL`; dependencies (the scheduler is very
/// chatty at debug) at info.
struct Logger;
impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        let ours = m.target().starts_with("cairn_trail") || m.target().starts_with("trail_core");
        m.level()
            <= if ours {
                LOG_LEVEL
            } else {
                log::LevelFilter::Info
            }
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            esp_println::println!("{:<5} {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}
static LOGGER: Logger = Logger;

static GNSS_EXECUTOR: StaticCell<InterruptExecutor<2>> = StaticCell::new();
static MAC_COPY: critical_section::Mutex<core::cell::Cell<[u8; 6]>> =
    critical_section::Mutex::new(core::cell::Cell::new([0; 6]));

/// The Wi-Fi MAC as read at boot.
pub fn mac_bytes() -> [u8; 6] {
    critical_section::with(|cs| MAC_COPY.borrow(cs).get())
}

pub fn mac_string() -> String<18> {
    let m = critical_section::with(|cs| MAC_COPY.borrow(cs).get());
    let mut s = String::new();
    let _ = core::fmt::write(
        &mut s,
        format_args!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        ),
    );
    s
}

#[allow(
    clippy::large_stack_frames,
    reason = "main sets up every peripheral once"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // SAFETY: set once, before any task runs.
    unsafe {
        let _ = log::set_logger_racy(&LOGGER);
        log::set_max_level_racy(LOG_LEVEL);
    }
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    // Wi-Fi buffers, TLS records and TCP windows need more than the reclaimed
    // bootloader RAM.
    esp_alloc::heap_allocator!(size: 128 * 1024);

    let timg0 = TimerGroup::new(p.TIMG0);
    esp_rtos::start(timg0.timer0, p.FROM_CPU_INTR0);

    let mut mac = [0u8; 6];
    mac.copy_from_slice(esp_hal::efuse::base_mac_address().as_bytes());
    critical_section::with(|cs| MAC_COPY.borrow(cs).set(mac));

    log::info!(
        "cairn-trail {} ({}) commit {}{} mac {}",
        shared::VERSION,
        shared::FLAVOUR,
        shared::COMMIT,
        shared::DIRTY,
        mac_string()
    );

    // The LoRa module's pins: inputs, no pull, so nothing feeds the unpowered
    // SX1262 (ALDO3 is never enabled). Held for the life of the program.
    let no_pull = InputConfig::default().with_pull(Pull::None);
    let _lora_pins = (
        Input::new(p.GPIO1, no_pull),
        Input::new(p.GPIO2, no_pull),
        Input::new(p.GPIO4, no_pull),
        Input::new(p.GPIO5, no_pull),
        Input::new(p.GPIO10, no_pull),
        Input::new(p.GPIO11, no_pull),
        Input::new(p.GPIO12, no_pull),
        Input::new(p.GPIO13, no_pull),
    );

    // PMU first: nothing else has power until it says so.
    let i2c_cfg = I2cConfig::default().with_frequency(Rate::from_khz(400));
    let i2c1 = I2c::new(p.I2C1, i2c_cfg)
        .unwrap()
        .with_sda(p.GPIO42)
        .with_scl(p.GPIO41);
    let mut pmu = pmu::Pmu::new(i2c1);
    match pmu.ic_type() {
        Ok(id) => event(
            "pmu",
            format_args!(
                "AXP2101 ic_type=0x{:02x} expected={}",
                id,
                id == pmu::IC_TYPE_AXP2101
            ),
        ),
        Err(_) => event("pmu", format_args!("AXP2101 did not answer")),
    }
    if let (Ok((dc, ldo)), Ok(v), Ok(k)) = (pmu.rails(), pmu.ldo_voltages(), pmu.power_key_timing())
    {
        event(
            "rails_as_found",
            format_args!(
                "dcdc=0x{:02x} ldo=0x{:02x} aldo1-4,bldo1-2={:?}mV key_timing=0x{:02x}",
                dc, ldo, v, k
            ),
        );
    }
    let mut delay = esp_hal::delay::Delay::new();
    let configured = pmu.configure(true, &mut delay).is_ok();
    // Identity and network settings, from the `cairn` flash partition.
    let provisioned = keys::init(esp_storage::FlashStorage::new(p.FLASH));
    event("keys", format_args!("{}", keys::summary()));
    let _ = provisioned;
    // The hardware TRNG (radio noise and the ADC) for TLS; held for the
    // program's life.
    let _trng_source = esp_hal::rng::TrngSource::new(p.RNG, p.ADC1);
    if let (Ok((dc, ldo)), Ok(off)) = (pmu.rails(), pmu.lora_rail_off()) {
        event(
            "rails",
            format_args!(
                "configured={} dcdc=0x{:02x} ldo=0x{:02x} lora_rail_off={}",
                configured, dc, ldo, off
            ),
        );
    }
    // Give the sensor, SD and GNSS rails a moment to settle.
    delay.delay_millis(50);

    // GNSS on its own interrupt-priority executor.
    let executor = GNSS_EXECUTOR.init(InterruptExecutor::new(p.FROM_CPU_INTR2));
    let gnss_spawner = executor.start(Priority::Priority2);
    gnss_spawner.spawn(gnss::gnss_task(p.UART1, p.GPIO8, p.GPIO9).unwrap());

    // I²C bus 0: barometer, magnetometer, display.
    let i2c0 = I2c::new(p.I2C0, i2c_cfg)
        .unwrap()
        .with_sda(p.GPIO17)
        .with_scl(p.GPIO18);
    spawner.spawn(sensors::sensors_task(i2c0).unwrap());

    // The shared SPI bus: SD card and IMU.
    let spi = Spi::new(
        p.SPI2,
        SpiConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .unwrap()
    .with_sck(p.GPIO36)
    .with_mosi(p.GPIO35)
    .with_miso(p.GPIO37);
    let sd_cs = Output::new(p.GPIO47, Level::High, OutputConfig::default());
    let imu_cs = Output::new(p.GPIO34, Level::High, OutputConfig::default());
    spawner.spawn(storage::storage_task(spi, sd_cs, imu_cs).unwrap());

    spawner.spawn(tasks::pmu_task(pmu).unwrap());
    spawner.spawn(
        tasks::button_task(Input::new(
            p.GPIO0,
            InputConfig::default().with_pull(Pull::Up),
        ))
        .unwrap(),
    );
    spawner.spawn(tasks::pps_task(Input::new(p.GPIO6, no_pull)).unwrap());

    let (rx, _tx) = UsbSerialJtag::new(p.USB_DEVICE).split();
    spawner.spawn(tasks::console_task(rx).unwrap());
    spawner.spawn(tasks::status_task().unwrap());

    // Wi-Fi and the check-in, if provisioned.
    net::start(&spawner, p.WIFI);

    event(
        "boot",
        format_args!(
            "cairn-trail {} ({}) {}{}",
            shared::VERSION,
            shared::FLAVOUR,
            shared::COMMIT,
            shared::DIRTY
        ),
    );

    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(3600)).await;
        // Keep the LoRa pins' configuration alive for the life of the program.
        let _ = &_lora_pins;
        let _ = &_trng_source;
    }
}
