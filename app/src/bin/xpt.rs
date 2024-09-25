#![no_std]
#![no_main]

use core::cell::RefCell;
use bit_field::{BitArray, BitField};
use display_interface_spi::SPIInterface;
use embassy_embedded_hal::shared_bus::blocking::spi::SpiDevice;
use embassy_executor::Spawner;
use embassy_sync::{
    blocking_mutex::{raw::NoopRawMutex, NoopMutex},
    signal::Signal,
};
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    mono_font::ascii,
    pixelcolor::Rgb565,
    prelude::{DrawTarget, Point, RgbColor},
};
use esp_backtrace as _;
use esp_hal::{
    clock::ClockControl,
    gpio,
    gpio::{GpioPin, Input, Io, Level, Output, Pull, NO_PIN},
    peripherals::{Peripherals, SPI2, SPI3},
    prelude::*,
    rtc_cntl::Rtc,
    spi::{master::Spi, FullDuplexMode, SpiMode},
    system::SystemControl,
    timer::{timg::TimerGroup, OneShotTimer},
};
use esp_println::println;
use kolibri_cyd_tester_app_embassy::Debouncer;
use kolibri_embedded_gui::{
    button::Button,
    label::Label,
    style::medsize_rgb565_style,
    ui::{Interaction, Ui},
};
use mipidsi::{
    models::{ILI9486Rgb565, ILI9486Rgb666},
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder,
};
use static_cell::{make_static, StaticCell};
use xpt2046::Xpt2046;

#[main]
async fn main(spawner: Spawner) {
    let peripherals = Peripherals::take();
    let system = SystemControl::new(peripherals.SYSTEM);
    let mut clocks = ClockControl::boot_defaults(system.clock_control).freeze();

    // Enable the RWDT watchdog timer:
    let mut rtc = Rtc::new(peripherals.LPWR);
    rtc.rwdt.set_timeout(2.secs());
    rtc.rwdt.enable();
    println!("RWDT watchdog enabled!");

    // Initialize the SYSTIMER peripheral, and then Embassy:
    let timg0 = TimerGroup::new(peripherals.TIMG0, &clocks);
    esp_hal_embassy::init(&clocks, timg0.timer0);
    println!("Embassy initialized!");

    let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);
    // let mut led = Output::new(io.pins.gpio5, Level::High);
    // Initialize SPI
    let sclk = io.pins.gpio14;
    let miso = io.pins.gpio12;
    let mosi = io.pins.gpio13;
    let cs = io.pins.gpio15;
    let dc = io.pins.gpio2;
    let mut backlight = Output::new(io.pins.gpio21, Level::Low);

    // Note: RST is not initialized as it's set to -1 in the instructions

    // up to 80MHz is possible (even tho the display driver isn't supposed to be
    // that fast) Dataseheet sais 10MHz, so we're gonna go with that
    let mut spi = Spi::new(peripherals.SPI2, 10.MHz(), SpiMode::Mode0, &mut clocks).with_pins(
        Some(sclk),
        Some(mosi),
        Some(miso),
        NO_PIN,
    );

    static DISP_SPI_BUS: StaticCell<NoopMutex<RefCell<Spi<SPI2, FullDuplexMode>>>> =
        StaticCell::new();
    let spi_bus = NoopMutex::new(RefCell::new(spi));
    let spi_bus = DISP_SPI_BUS.init(spi_bus);

    let di = SPIInterface::new(
        SpiDevice::new(spi_bus, Output::new(cs, Level::Low)),
        Output::new(dc, Level::Low),
    );
    let mut display = Builder::new(ILI9486Rgb565, di)
        .orientation(Orientation {
            rotation: Rotation::Deg90,
            mirrored: true,
        })
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut embassy_time::Delay)
        .unwrap();

    {
        let mut ui = Ui::new_fullscreen(&mut display, medsize_rgb565_style());
        ui.clear_background().ok();
    }
    backlight.set_high();

    // init touchscreen pins
    let touch_irq = io.pins.gpio36;
    let touch_mosi = io.pins.gpio32;
    let touch_miso = io.pins.gpio39;
    let touch_clk = io.pins.gpio25;
    let touch_cs = io.pins.gpio33;

    let mut touch_spi = Spi::new(peripherals.SPI3, 2.MHz(), SpiMode::Mode0, &mut clocks)
        .with_pins(Some(touch_clk), Some(touch_mosi), Some(touch_miso), NO_PIN);
    // static TOUCH_SPI_BUS: StaticCell<NoopMutex<RefCell<Spi<SPI3,
    // FullDuplexMode>>>> =     StaticCell::new();
    // let touch_spi_bus = NoopMutex::new(RefCell::new(touch_spi));
    // let touch_spi_bus = TOUCH_SPI_BUS.init(touch_spi_bus);

    let touch_signal = Signal::new();
    static TOUCH_SIGNAL: StaticCell<Signal<NoopRawMutex, Option<Point>>> = StaticCell::new();
    let touch_signal = &*TOUCH_SIGNAL.init(touch_signal);

    // init RGB LED pins

    let mut red_led = Output::new(io.pins.gpio4, Level::High);
    let mut green_led = Output::new(io.pins.gpio16, Level::High);
    let mut blue_led = Output::new(io.pins.gpio17, Level::High);

    // TODO: Spawn some tasks
    let _ = spawner;

    static BUF_CELL: StaticCell<[Rgb565; 100 * 100]> = StaticCell::new();
    let buf = BUF_CELL.init([Rgb565::BLACK; 100 * 100]);

    // Periodically feed the RWDT watchdog timer when our tasks are not running:
    loop {
        // XPT read raw
        let mut input = [0x91, 0x00, 0x00]; // measure X pos
        touch_spi.transfer(&mut input).unwrap();
        let mut input = [0x91, 0x00, 0x00]; // measure X pos
        touch_spi.transfer(&mut input).unwrap();

        let mut input = [0x91, 0x00, 0x00]; // measure X pos
        touch_spi.transfer(&mut input).unwrap();
        // println!("read: {:08b} {:08b} {:08b}", input[0], input[1], input[2]);
        let x = u16::from_be_bytes([input[1], input[2]]) >> 3;
        // println!("{:16b} ({})", num, num);

        let mut input = [0xD1, 0x00, 0x00]; // measure y pos
        touch_spi.transfer(&mut input).unwrap();
        let mut input = [0xD1, 0x00, 0x00]; // measure y pos
        touch_spi.transfer(&mut input).unwrap();
        let mut input = [0xD1, 0x00, 0x00]; // measure y pos
        touch_spi.transfer(&mut input).unwrap();
        // println!("read: {:08b} {:08b} {:08b}", input[0], input[1], input[2]);
        let y = u16::from_be_bytes([input[1], input[2]]) >> 3;
        // println!("{:16b} ({})", num, num);
        println!("x: {}, y: {}", (x as f32 * 0.078125) as u16, (y as f32 * 0.05859375) as u16);



        rtc.rwdt.feed();
        Timer::after(Duration::from_millis(17)).await; // 60 a second
    }
}
