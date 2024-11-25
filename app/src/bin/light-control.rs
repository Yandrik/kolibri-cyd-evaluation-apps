#![no_std]
#![no_main]

use core::{cell::RefCell, cmp::min, str::FromStr};

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
    prelude::{Point, RgbColor, Size, WebColors},
};
use embedded_graphics_profiler_display::ProfilerDisplay;
use esp_backtrace as _;
use esp_hal::{
    clock::ClockControl,
    gpio::{
        GpioPin,
        Input,
        Io,
        Level,
        Output,
        Pull,
        NO_PIN,
    },
    peripherals::{Peripherals, SPI2, SPI3},
    prelude::*,
    rtc_cntl::Rtc,
    spi::{master::Spi, FullDuplexMode, SpiMode},
    system::SystemControl,
    timer::timg::TimerGroup,
};
use esp_println::println;
use kolibri_embedded_gui::{
    iconbutton::IconButton,
    icons::size32px,
    label::{HashLabel, Hasher, Label},
    slider::Slider,
    smartstate::SmartstateProvider,
    spacer::Spacer,
    style::medsize_rgb565_style,
    toggle_switch::ToggleSwitch,
    ui::{Interaction, Ui},
};
use mipidsi::{
    models::ILI9341Rgb565,
    options::{ColorOrder, Orientation, Rotation},
    Builder,
};
use static_cell::StaticCell;
use xpt2046::Xpt2046;

#[embassy_executor::task]
async fn touch_task(
    touch_irq: GpioPin<36>,
    spi: &'static mut NoopMutex<
        RefCell<Spi<'static, SPI3, FullDuplexMode>>,
    >,
    touch_cs: GpioPin<33>,
    touch_signal: &'static Signal<
        NoopRawMutex,
        Option<Point>,
    >,
) -> ! {
    let mut touch_driver = Xpt2046::new(
        SpiDevice::new(
            spi,
            Output::new(touch_cs, Level::Low),
        ),
        Input::new(touch_irq, Pull::Up),
        xpt2046::Orientation::LandscapeFlipped,
    );
    touch_driver.set_num_samples(16);
    touch_driver.init(&mut embassy_time::Delay).unwrap();

    println!("touch task");

    loop {
        touch_driver
            .run()
            .expect("Running Touch driver failed");
        if touch_driver.is_touched() {
            let point = touch_driver.get_touch_point();
            touch_signal.signal(Some(Point::new(
                point.x + 25,
                240 - point.y,
            )));
        } else {
            touch_signal.signal(None);
        }
        Timer::after(Duration::from_millis(1)).await; // 100
                                                      // a second

        // Your touch handling logic here
    }
}

fn lerp_fixed(start: u8, end: u8, t: u8, max_t: u8) -> u8 {
    let (start, end, t, max_t) =
        (start as u16, end as u16, t as u16, max_t as u16);
    let t = t.min(max_t);
    let result = start
        + ((end - start.min(end)) * t + (max_t / 2))
            / max_t;
    result as u8
}

#[derive(Debug, Clone)]
struct Lamp {
    pub name: heapless::String<64>,
    pub on: bool,
    pub brightness: i16,
}

impl Lamp {
    pub fn new(name: &str) -> Self {
        Self {
            name: heapless::String::from_str(name).unwrap(),
            on: false,
            brightness: 255,
        }
    }
}

enum Page {
    Home,
    LampCtrl(usize),
}
struct AppData {
    lamps: heapless::Vec<Lamp, 8>,
}

impl AppData {
    fn new() -> Self {
        Self {
            lamps: heapless::Vec::new(),
        }
    }

    fn add_lamp(&mut self, name: &str) {
        self.lamps.push(Lamp::new(name)).unwrap();
    }
}

#[main]
async fn main(spawner: Spawner) {
    let peripherals = Peripherals::take();
    let system = SystemControl::new(peripherals.SYSTEM);
    let mut clocks =
        ClockControl::boot_defaults(system.clock_control)
            .freeze();

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
    // let mut led = Output::new(io.pins.gpio5,
    // Level::High); Initialize SPI
    let sclk = io.pins.gpio14;
    let miso = io.pins.gpio12;
    let mosi = io.pins.gpio13;
    let cs = io.pins.gpio15;
    let dc = io.pins.gpio2;
    let mut backlight =
        Output::new(io.pins.gpio21, Level::Low);

    // Note: RST is not initialized as it's set to -1 in the
    // instructions

    // up to 80MHz is possible (even tho the display driver
    // isn't supposed to be that fast) Dataseheet sais
    // 10MHz, so we're gonna go with that
    let mut spi = Spi::new(
        peripherals.SPI2,
        10.MHz(),
        SpiMode::Mode0,
        &mut clocks,
    )
    .with_pins(
        Some(sclk),
        Some(mosi),
        Some(miso),
        NO_PIN,
    );

    static DISP_SPI_BUS: StaticCell<
        NoopMutex<RefCell<Spi<SPI2, FullDuplexMode>>>,
    > = StaticCell::new();
    let spi_bus = NoopMutex::new(RefCell::new(spi));
    let spi_bus = DISP_SPI_BUS.init(spi_bus);

    let di = SPIInterface::new(
        SpiDevice::new(
            spi_bus,
            Output::new(cs, Level::Low),
        ),
        Output::new(dc, Level::Low),
    );
    let display = Builder::new(ILI9341Rgb565, di)
        .orientation(Orientation {
            rotation: Rotation::Deg90,
            mirrored: true,
        })
        .color_order(ColorOrder::Bgr)
        // .invert_colors(ColorInversion::Inverted)
        .init(&mut embassy_time::Delay)
        .unwrap();

    let mut display = ProfilerDisplay::new(display);
    let style = medsize_rgb565_style();

    {
        let mut ui =
            Ui::new_fullscreen(&mut display, style);
        ui.clear_background().ok();
    }
    backlight.set_high();

    // init touchscreen pins
    let touch_irq = io.pins.gpio36;
    let touch_mosi = io.pins.gpio32;
    let touch_miso = io.pins.gpio39;
    let touch_clk = io.pins.gpio25;
    let touch_cs = io.pins.gpio33;

    // 2MHz is the MAX! DO NOT DECREASE! This is really
    // important.
    let mut touch_spi = Spi::new(
        peripherals.SPI3,
        2.MHz(),
        SpiMode::Mode0,
        &mut clocks,
    )
    .with_pins(
        Some(touch_clk),
        Some(touch_mosi),
        Some(touch_miso),
        NO_PIN,
    );
    static TOUCH_SPI_BUS: StaticCell<
        NoopMutex<RefCell<Spi<SPI3, FullDuplexMode>>>,
    > = StaticCell::new();
    let touch_spi_bus =
        NoopMutex::new(RefCell::new(touch_spi));
    let touch_spi_bus = TOUCH_SPI_BUS.init(touch_spi_bus);

    let touch_signal = Signal::new();
    static TOUCH_SIGNAL: StaticCell<
        Signal<NoopRawMutex, Option<Point>>,
    > = StaticCell::new();
    let touch_signal = &*TOUCH_SIGNAL.init(touch_signal);

    spawner
        .spawn(touch_task(
            touch_irq,
            touch_spi_bus,
            touch_cs,
            touch_signal,
        ))
        .unwrap();

    // TODO: Spawn some tasks
    let _ = spawner;

    // variables
    let mut appdata = AppData::new();

    appdata.add_lamp("Front Door");
    appdata.add_lamp("Living Room");
    // appdata.lamps[1].on = true;
    appdata.add_lamp("Bedroom");
    // appdata.lamps[2].on = true;
    appdata.add_lamp("Bathroom");
    appdata.add_lamp("Porch");

    let mut cur_page = Page::Home;

    // touchpoints

    let mut last_touch = None;

    static BUF_CELL: StaticCell<[Rgb565; 200 * 100]> =
        StaticCell::new();
    let buf = BUF_CELL.init([Rgb565::BLACK; 200 * 100]);

    let mut textbuf = [0u8; 64];
    let hasher = Hasher::new();

    // Periodically feed the RWDT watchdog timer when our
    // tasks are not running:
    let mut sm = SmartstateProvider::<20>::new();
    loop {
        // SMART REDRAWING ENABLE / DISABLE
        // sm.force_redraw_all();

        let start_time = embassy_time::Instant::now();
        sm.restart_counter();
        let mut ui =
            Ui::new_fullscreen(&mut display, style);
        if let Some(touch) = touch_signal.try_take() {
            let interact = match (touch, last_touch) {
                (Some(point), Some(_)) => {
                    Interaction::Drag(point)
                }
                (Some(point), None) => {
                    Interaction::Click(point)
                }
                (None, Some(point)) => {
                    Interaction::Release(point)
                }
                (None, None) => Interaction::None,
            };
            ui.interact(interact);
            // println!("{:?}, {:?}, {:?}", last_touch,
            // touch, interact);

            last_touch = touch;
        }

        // BUFFER ENABLE/DISABLE
        ui.set_buffer(buf);

        let start_draw_time = embassy_time::Instant::now();
        if let Page::Home = cur_page {
            ui.add_centered(
                HashLabel::new(
                    "Light Control App (Kolibri)",
                    sm.next(),
                    &hasher,
                )
                // .smartstate(sm.next())
                .with_font(ascii::FONT_9X18_BOLD),
            );
        }

        match cur_page {
            Page::Home => {
                for (i, lamp) in
                    appdata.lamps.iter().enumerate()
                {
                    let mut break_loop = false;
                    ui.sub_ui(|ui| {
                        ui.style_mut().icon_color = if lamp.on {
                            Rgb565::CSS_GOLD
                        } else {
                            Rgb565::WHITE
                        };
                        if ui
                            .add_horizontal(
                                IconButton::new(size32px::home::LightBulb)
                                    .label(lamp.name.as_str())
                                    .smartstate(sm.next()),
                            )
                            .clicked()
                        {
                            cur_page = Page::LampCtrl(i);
                            ui.clear_background().ok();
                            sm.force_redraw_all();
                            break_loop = true;
                        }
                        Ok(())
                    })
                    .ok();
                    if break_loop {
                        break;
                    }
                    if i % 3 == 2 {
                        ui.new_row();
                    }
                }
            }
            Page::LampCtrl(lamp) => {
                let lamp = &mut appdata.lamps[lamp];
                if ui
                    .add_horizontal(
                        IconButton::new(size32px::navigation::NavArrowLeft).smartstate(sm.next()),
                    )
                    .clicked()
                {
                    cur_page = Page::Home;
                    ui.clear_background().ok();
                    sm.force_redraw_all();
                    continue;
                }
                ui.add_horizontal(Spacer::new(Size::new(
                    30, 0,
                )));
                ui.add(
                    Label::new(lamp.name.as_str())
                        .smartstate(sm.next())
                        .with_font(ascii::FONT_9X18_BOLD),
                );
                ui.add(Spacer::new(Size::new(0, 20)));
                ui.add_centered(
                    Slider::new(
                        &mut lamp.brightness,
                        0..=255,
                    )
                    .width(300)
                    .label("Brightness")
                    .smartstate(sm.next()),
                );
                ui.add(Spacer::new(Size::new(0, 10)));
                ui.add_centered(
                    ToggleSwitch::new(&mut lamp.on)
                        .smartstate(sm.next()),
                );
                ui.add_centered(
                    Label::new("Turn on/off")
                        .smartstate(sm.next()),
                );
            }
        }

        let end_time = embassy_time::Instant::now();
        let draw_time = display.get_time();
        let prep_time = start_draw_time - start_time;
        let proc_time = end_time - start_draw_time;
        let proc_time =
            proc_time - min(draw_time, proc_time);
        rtc.rwdt.feed();

        if draw_time.as_micros() > 0 {
            println!(
                "draw time: {}.{:03}ms | prep time: {}.{:03}ms | proc time: {}.{:03}ms | total time: {}.{:03}ms",
                draw_time.as_millis(),
                draw_time.as_micros() % 100,
                prep_time.as_millis(),
                prep_time.as_micros() % 100,
                proc_time.as_millis(),
                proc_time.as_micros() % 100,
                (draw_time + prep_time + proc_time).as_millis(),
                (draw_time + prep_time + proc_time).as_micros() % 100, );
        }
        display.reset_time();
        Timer::after(Duration::from_millis(17)).await; // 60
                                                       // a second
    }
}
