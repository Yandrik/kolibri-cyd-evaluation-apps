#![no_std]
#![no_main]

use core::cell::RefCell;
use core::cmp::min;
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
use embedded_graphics_profiler_display::ProfilerDisplay;
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
    smartstate::SmartstateProvider,
    style::medsize_rgb565_style,
    ui::{Interaction, Ui},
};
use kolibri_embedded_gui::helpers::keyboard::draw_keyboard;
use mipidsi::{
    models::{ILI9486Rgb565, ILI9486Rgb666},
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder,
};
use static_cell::{make_static, StaticCell};
use xpt2046::Xpt2046;

#[embassy_executor::task]
async fn touch_task(
    touch_irq: GpioPin<36>,
    spi: &'static mut NoopMutex<RefCell<Spi<'static, SPI3, FullDuplexMode>>>,
    touch_cs: GpioPin<33>,
    touch_signal: &'static Signal<NoopRawMutex, Option<Point>>,
) -> ! {
    let mut touch_driver = Xpt2046::new(
        SpiDevice::new(spi, Output::new(touch_cs, Level::Low)),
        Input::new(touch_irq, Pull::Up),
        xpt2046::Orientation::LandscapeFlipped,
    );
    touch_driver.set_num_samples(16);
    touch_driver.init(&mut embassy_time::Delay).unwrap();

    let mut debounce = Debouncer::new();
    println!("touch task");

    loop {
        touch_driver.run().expect("Running Touch driver failed");
        if touch_driver.is_touched() {
            let point = touch_driver.get_touch_point();
            touch_signal.signal(Some(Point::new(point.x + 25, 240 - point.y)));
        } else {
            touch_signal.signal(None);
        }
        Timer::after(Duration::from_millis(1)).await; // 100 a second

        // Your touch handling logic here
    }
}

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
    let display = Builder::new(ILI9486Rgb565, di)
        .orientation(Orientation {
            rotation: Rotation::Deg90,
            mirrored: true,
        })
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut embassy_time::Delay)
        .unwrap();

    let mut display = ProfilerDisplay::new(display);

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

    // 2MHz is the MAX! DO NOT DECREASE! This is really important.
    let mut touch_spi = Spi::new(peripherals.SPI3, 2.MHz(), SpiMode::Mode0, &mut clocks).with_pins(
        Some(touch_clk),
        Some(touch_mosi),
        Some(touch_miso),
        NO_PIN,
    );
    static TOUCH_SPI_BUS: StaticCell<NoopMutex<RefCell<Spi<SPI3, FullDuplexMode>>>> =
        StaticCell::new();
    let touch_spi_bus = NoopMutex::new(RefCell::new(touch_spi));
    let touch_spi_bus = TOUCH_SPI_BUS.init(touch_spi_bus);

    let touch_signal = Signal::new();
    static TOUCH_SIGNAL: StaticCell<Signal<NoopRawMutex, Option<Point>>> = StaticCell::new();
    let touch_signal = &*TOUCH_SIGNAL.init(touch_signal);

    spawner
        .spawn(touch_task(touch_irq, touch_spi_bus, touch_cs, touch_signal))
        .unwrap();

    // init RGB LED pins

    let mut red_led = Output::new(io.pins.gpio4, Level::High);
    let mut green_led = Output::new(io.pins.gpio16, Level::High);
    let mut blue_led = Output::new(io.pins.gpio17, Level::High);

    // init SD card pins
    let sd_miso = io.pins.gpio19;
    let sd_mosi = io.pins.gpio23;
    let sd_sck = io.pins.gpio18;
    let sd_cs = io.pins.gpio5;

    // TODO: Spawn some tasks
    let _ = spawner;

    let mut last_touch = None;

    static BUF_CELL: StaticCell<[Rgb565; 100 * 100]> = StaticCell::new();
    let buf = BUF_CELL.init([Rgb565::BLACK; 100 * 100]);

    // Periodically feed the RWDT watchdog timer when our tasks are not running:
    let mut sm = SmartstateProvider::<20>::new();
    loop {
        let start_time = embassy_time::Instant::now();
        sm.restart_counter();
        let mut ui = Ui::new_fullscreen(&mut display, medsize_rgb565_style());
        ui.set_buffer(buf);
        if let Some(touch) = touch_signal.try_take() {
            let interact = match (touch, last_touch) {
                (Some(point), Some(_)) => Interaction::Drag(point),
                (Some(point), None) => Interaction::Click(point),
                (None, Some(point)) => Interaction::Release(point),
                (None, None) => Interaction::None,
            };
            ui.interact(interact);
            // println!("{:?}, {:?}, {:?}", last_touch, touch, interact);
            
            last_touch = touch;
        }
        let start_draw_time = embassy_time::Instant::now();
        ui.sub_ui(|ui| {
            ui.style_mut().default_font = ascii::FONT_9X18_BOLD;
            ui.add(Label::new("Kolibri Tester").smartstate(sm.next()));
            Ok(())
        })
        .ok();
        ui.add_horizontal(Button::new("Works!").smartstate(sm.next()));
        ui.add(Button::new("And pretty nicely!").smartstate(sm.next()));
        let end_time = embassy_time::Instant::now();
        let draw_time = display.get_time();
        let prep_time = start_draw_time - start_time;
        let proc_time = end_time - start_draw_time;
        let proc_time = proc_time - min(draw_time, proc_time);
        rtc.rwdt.feed();

        if draw_time.as_micros() > 0 {
            println!(
                "draw time: {}.{:03}ms | prep time: {}.{:03}ms | proc time: {}.{:03}ms ",
                draw_time.as_millis(),
                draw_time.as_micros() % 100,
                prep_time.as_millis(),
                prep_time.as_micros() % 100,
                proc_time.as_millis(),
                proc_time.as_micros() % 100,
            );

        }
        display.reset_time();
        Timer::after(Duration::from_millis(17)).await; // 60 a second
    }
}
