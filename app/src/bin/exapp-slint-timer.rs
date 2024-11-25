#![no_std]
#![no_main]

extern crate alloc;

use core::{cell::RefCell, cmp::min, fmt};

use display_interface_spi::SPIInterface;
use embassy_embedded_hal::shared_bus::blocking::spi::SpiDevice;
use embassy_executor::Spawner;
use embassy_sync::{
    blocking_mutex::{raw::NoopRawMutex, NoopMutex},
    signal::Signal,
};
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::{
    mono_font::ascii,
    pixelcolor::Rgb565,
    prelude::{DrawTarget, Point, RgbColor, Size, WebColors},
};
use embedded_graphics_profiler_display::ProfilerDisplay;
use esp_backtrace as _;
use esp_hal::{
    clock::ClockControl,
    gpio::{GpioPin, Input, Io, Level, Output, Pull, NO_PIN},
    peripherals::{Peripherals, SPI2, SPI3},
    prelude::*,
    rtc_cntl::Rtc,
    spi::{master::Spi, FullDuplexMode, SpiMode},
    system::SystemControl,
    timer::timg::TimerGroup,
};
use esp_println::println;
use kolibri_cyd_tester_app_embassy::Debouncer;
use kolibri_embedded_gui::{
    button::Button,
    checkbox::Checkbox,
    icon::IconWidget,
    iconbutton::IconButton,
    icons::{size12px, size24px, size32px, size48px, size96px},
    label::Label,
    smartstate::SmartstateProvider,
    spacer::Spacer,
    style::medsize_rgb565_style,
    ui::{Interaction, Ui},
};
use mipidsi::{
    models::ILI9486Rgb565,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder,
};
use slint::platform::software_renderer::MinimalSoftwareWindow;
use static_cell::StaticCell;
use xpt2046::Xpt2046;

slint::include_modules!();

use esp_alloc as _;

fn init_heap() {
    const HEAP_SIZE: usize = 32 * 1024;
    static mut HEAP: MaybeUninit<[u8; HEAP_SIZE]> = MaybeUninit::uninit();

    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            HEAP.as_mut_ptr() as *mut u8,
            HEAP_SIZE,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }
}

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

struct AppData {
    timer_start: Instant,
    timer_set_duration: Duration,
    timer_remaining_duration: Duration,
    timer_running: bool,
    timer_paused: bool,
}

impl AppData {
    fn new() -> Self {
        Self {
            timer_start: Instant::now(),
            timer_set_duration: Duration::from_secs(10),
            timer_remaining_duration: Duration::from_secs(10),
            timer_running: false,
            timer_paused: false,
        }
    }

    fn set_timer_duration(&mut self, duration: Duration) {
        self.timer_set_duration = duration;
    }

    fn add_secs(&mut self, secs: u64) {
        self.set_timer_duration(
            self.timer_set_duration
                .checked_add(Duration::from_secs(secs))
                .unwrap_or(Duration::from_secs(5999))
                .min(Duration::from_secs(5999)),
        );
    }

    fn sub_secs(&mut self, secs: u64) {
        self.set_timer_duration(
            self.timer_set_duration
                .checked_sub(Duration::from_secs(secs))
                .unwrap_or(Duration::from_secs(10))
                .max(Duration::from_secs(10)),
        );
    }

    fn start_timer(&mut self) {
        if !self.timer_paused {
            self.timer_remaining_duration = self.timer_set_duration;
        }

        self.timer_start = Instant::now();
        self.timer_running = true;
        self.timer_paused = false;
    }

    fn pause_timer(&mut self) {
        self.timer_paused = true;
        self.timer_remaining_duration = self
            .timer_remaining_duration
            .checked_sub(self.timer_start.elapsed())
            .unwrap_or(Duration::from_secs(0));
    }

    fn reset_timer(&mut self) {
        self.timer_start = Instant::now();
        self.timer_paused = false;
        self.timer_running = false;
        self.timer_remaining_duration = self.timer_set_duration;
    }

    fn remaining(&self) -> Duration {
        if self.timer_stopped() {
            self.timer_set_duration
        } else if self.timer_paused() {
            self.timer_remaining_duration
        } else {
            self.timer_remaining_duration
                .checked_sub(self.timer_start.elapsed())
                .unwrap_or(Duration::from_secs(0))
        }
    }

    fn timer_stopped(&self) -> bool {
        !self.timer_running
    }

    fn timer_paused(&self) -> bool {
        self.timer_running && self.timer_paused
    }

    fn timer_running(&self) -> bool {
        self.timer_running && !self.timer_paused
    }

    fn timer_finished(&self) -> bool {
        self.timer_running && self.remaining() == Duration::from_secs(0)
    }
}

struct MyPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for MyPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        // Since on MCUs, there can be only one window, just return a clone of self.window.
        // We'll also use the same window in the event loop.
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_micros(self.timer.get_time())
    }
    // optional: You can put the event loop there, or in the main function, see later
    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        todo!();
    }
}

#[main]
async fn main(spawner: Spawner) {

    init_heap();

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

    let window = MinimalSoftwareWindow::new(Default::default());
    slint::platform::set_platform(Box::new(MyPlatform {
        window: window.clone(),
        //...
    }))
    .unwrap();

    let ui = 


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

    // TODO: Spawn some tasks
    let _ = spawner;

    // variables
    let mut appdata = AppData::new();
    let (mut prev_mins, mut prev_secs, mut prev_millis) = (0, 0, 0);
    let mut finished = false;

    // touchpoints

    let mut last_touch = None;

    static BUF_CELL: StaticCell<[Rgb565; 100 * 100]> = StaticCell::new();
    let buf = BUF_CELL.init([Rgb565::BLACK; 100 * 100]);

    let mut textbuf = [0u8; 64];

    // Periodically feed the RWDT watchdog timer when our tasks are not running:
    let mut sm = SmartstateProvider::<20>::new();
    loop {
        // SMART REDRAWING ENABLE / DISABLE
        // sm.force_redraw_all();

        let start_time = embassy_time::Instant::now();
        sm.restart_counter();
        let mut ui = Ui::new_fullscreen(&mut display, medsize_rgb565_style());
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

        // BUFFER ENABLE/DISABLE
        ui.set_buffer(buf);

        let start_draw_time = embassy_time::Instant::now();
        ui.sub_ui(|ui| {
            ui.style_mut().default_font = ascii::FONT_9X18_BOLD;
            ui.add(Label::new("Kolibri Timer App").smartstate(sm.next()));
            Ok(())
        })
        .ok();

        let remaining = appdata.remaining();

        ui.add(Spacer::new(Size::new(0, 60)));
        ui.add_horizontal(Spacer::new(Size::new(80, 0)));
        ui.sub_ui(|ui| {
            ui.style_mut().default_font = ascii::FONT_10X20;
            if remaining.as_secs() / 60 != prev_mins {
                sm.peek().force_redraw();
                prev_mins = remaining.as_secs() / 60;
            }
            ui.add_horizontal(
                Label::new(
                    &format_no_std::show(
                        &mut textbuf,
                        format_args!("{:02}", remaining.as_secs() / 60),
                    )
                    .unwrap(),
                )
                .smartstate(sm.next()),
            );
            ui.add_horizontal(Label::new(":").smartstate(sm.next()));

            if remaining.as_secs() % 60 != prev_secs {
                sm.peek().force_redraw();
                prev_secs = remaining.as_secs() % 60;
            }
            ui.add_horizontal(
                Label::new(
                    &format_no_std::show(
                        &mut textbuf,
                        format_args!("{:02}", remaining.as_secs() % 60),
                    )
                    .unwrap(),
                )
                .smartstate(sm.next()),
            );
            ui.add_horizontal(Label::new(":").smartstate(sm.next()));

            if remaining.as_millis() % 1000 != prev_millis {
                sm.peek().force_redraw();
                prev_millis = remaining.as_millis() % 1000;
            }
            ui.add(
                Label::new(
                    &format_no_std::show(
                        &mut textbuf,
                        format_args!("{:03}", remaining.as_millis() % 1000),
                    )
                    .unwrap(),
                )
                .smartstate(sm.next()),
            );
            Ok(())
        })
        .ok();

        ui.add_horizontal(Spacer::new(Size::new(65, 0)));
        ui.sub_ui(|ui| {
            if appdata.timer_running() {
                ui.style_mut().icon_color = Rgb565::CSS_LIGHT_GRAY;
            }
            if ui
                .add_horizontal(IconButton::new(size32px::actions::AddCircle).smartstate(sm.next()))
                .clicked()
            {
                if !appdata.timer_running() {
                    appdata.add_secs(10);
                }
            }
            ui.add_horizontal(Label::new("+/- 10s").smartstate(sm.next()));
            if ui
                .add(IconButton::new(size32px::actions::MinusCircle).smartstate(sm.next()))
                .clicked()
            {
                if !appdata.timer_running() {
                    appdata.sub_secs(10);
                }
            }
            Ok(())
        })
        .ok();

        ui.add_horizontal(Spacer::new(Size::new(80, 0)));
        if ui
            .add_horizontal(IconButton::new(size48px::actions::Undo).smartstate(sm.next()))
            .clicked()
        {
            appdata.reset_timer();
            sm.force_redraw_all();
        }

        if !finished && appdata.timer_finished() {
            sm.peek().force_redraw();
        }

        if appdata.timer_finished() {
            if ui
                .add_horizontal(
                    IconButton::new(size48px::actions::RemoveSquare).smartstate(sm.next()),
                )
                .clicked()
            {
                appdata.reset_timer();
                sm.force_redraw_all();
            }
        } else if appdata.timer_running() {
            if ui
                .add_horizontal(IconButton::new(size48px::music::Pause).smartstate(sm.next()))
                .clicked()
            {
                appdata.pause_timer();
                sm.force_redraw_all();
            }
        } else {
            if ui
                .add_horizontal(IconButton::new(size48px::music::Play).smartstate(sm.next()))
                .clicked()
            {
                appdata.start_timer();
                sm.force_redraw_all();
            }
        }

        finished = appdata.timer_finished();

        let end_time = embassy_time::Instant::now();
        let draw_time = display.get_time();
        let prep_time = start_draw_time - start_time;
        let proc_time = end_time - start_draw_time;
        let proc_time = proc_time - min(draw_time, proc_time);
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
                (draw_time + prep_time + proc_time).as_micros() % 100,            );
        }
        display.reset_time();
        Timer::after(Duration::from_millis(17)).await; // 60 a second
    }
}
