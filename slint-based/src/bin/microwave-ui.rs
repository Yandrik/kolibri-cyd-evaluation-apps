#![no_std]
#![no_main]
extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::mem::MaybeUninit;
use core::{cell::RefCell, cmp::min};
use display_interface_spi::SPIInterface;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Delay;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics_core::pixelcolor::Rgb565;
use embedded_graphics_core::prelude::*;
use embedded_graphics_core::primitives::Rectangle;
use embedded_graphics_profiler_display::ProfilerDisplay;
use embedded_hal::digital::OutputPin;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{self, prelude::*};
use esp_hal::{
    clock::ClockControl,
    gpio::{
        GpioPin, Input, Io, Level, Output, Pull, NO_PIN,
    },
    peripherals::{Peripherals, SPI3},
    prelude::*,
    rtc_cntl::Rtc,
    spi::{master::Spi, FullDuplexMode, SpiMode},
    system::SystemControl,
    timer::timg::TimerGroup,
};
use esp_println::println;
use mipidsi::{
    options::{ColorOrder, Orientation, Rotation},
    Builder,
};
use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::platform::{
    Platform, PointerEventButton, WindowEvent,
};
use slint::{format, LogicalPosition};
use static_cell::StaticCell;
use xpt2046::Xpt2046;

use mipidsi::models::ILI9341Rgb565;
// slint::slint!{ export MyUI := Window {} }
/*
slint::include_modules!();
# */

slint::include_modules!();

fn init_heap() {
    const HEAP_SIZE: usize = 64 * 1024;
    static mut HEAP: MaybeUninit<[u8; HEAP_SIZE]> =
        MaybeUninit::uninit();

    unsafe {
        esp_alloc::HEAP.add_region(
            esp_alloc::HeapRegion::new(
                HEAP.as_mut_ptr() as *mut u8,
                HEAP_SIZE,
                esp_alloc::MemoryCapability::Internal
                    .into(),
            ),
        );
    }
}

#[embassy_executor::task]
async fn touch_task(
    touch_irq: GpioPin<36>,
    spi: ExclusiveDevice<
        Spi<'static, SPI3, FullDuplexMode>,
        Output<'static, GpioPin<33>>,
        &'static mut Delay,
    >,
    touch_signal: &'static Signal<
        CriticalSectionRawMutex,
        Option<Point>,
    >,
) -> ! {
    let mut touch_driver = Xpt2046::new(
        spi,
        Input::new(touch_irq, Pull::Up),
        xpt2046::Orientation::LandscapeFlipped,
    );
    touch_driver.set_num_samples(1);
    touch_driver.init(&mut embassy_time::Delay).unwrap();

    esp_println::println!("touch task");

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
        Timer::after(Duration::from_millis(1)).await; // 100 a second

        // Your touch handling logic here
    }
}

fn point_to_logical_pos(point: Point) -> LogicalPosition {
    LogicalPosition::new(point.x as f32, point.y as f32)
}

struct CYDPlatform {
    window: Rc<slint::platform::software_renderer::MinimalSoftwareWindow>,
}

impl Platform for CYDPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<
        Rc<dyn slint::platform::WindowAdapter>,
        slint::PlatformError,
    > {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> core::time::Duration {
        embassy_time::Instant::from_millis(0)
            .elapsed()
            .into()
    }

    //noinspection DuplicatedCode
    fn run_event_loop(
        &self,
    ) -> Result<(), slint::PlatformError> {
        todo!();
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
            timer_remaining_duration: Duration::from_secs(
                10,
            ),
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
            self.timer_remaining_duration =
                self.timer_set_duration;
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
        self.timer_remaining_duration =
            self.timer_set_duration;
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
        self.timer_running
            && self.remaining() == Duration::from_secs(0)
    }
}

#[main]
async fn main(spawner: Spawner) {
    init_heap();
    let peripherals = Peripherals::take();
    let system = SystemControl::new(peripherals.SYSTEM);
    let mut clocks =
        ClockControl::boot_defaults(system.clock_control)
            .freeze();

    let mut rtc = Rtc::new(peripherals.LPWR);
    rtc.rwdt.disable();
    let mut timer_group0 =
        TimerGroup::new(peripherals.TIMG0, &clocks);
    timer_group0.wdt.disable();
    let mut timer_group1 =
        TimerGroup::new(peripherals.TIMG1, &clocks);
    timer_group1.wdt.disable();

    esp_hal_embassy::init(&clocks, timer_group0.timer0);

    let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);

    let touch_irq = io.pins.gpio36;
    let touch_mosi = io.pins.gpio32;
    let touch_miso = io.pins.gpio39;
    let touch_clk = io.pins.gpio25;
    let touch_cs = io.pins.gpio33;

    // 2MHz is the MAX! DO NOT DECREASE! This is really important.
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

    static TOUCH_DELAY_STATICCELL: StaticCell<Delay> =
        StaticCell::new();
    let mut delay = TOUCH_DELAY_STATICCELL.init(Delay);

    let touch_spi = ExclusiveDevice::new(
        touch_spi,
        Output::new(touch_cs, Level::Low),
        delay,
    )
    .unwrap();

    let touch_signal = Signal::new();
    static TOUCH_SIGNAL: StaticCell<
        Signal<CriticalSectionRawMutex, Option<Point>>,
    > = StaticCell::new();
    let touch_signal = &*TOUCH_SIGNAL.init(touch_signal);

    // let sw_int = system.software_interrupt_control.software_interrupt2;

    // static EXECUTOR: StaticCell<InterruptExecutor<2>> = StaticCell::new();
    // let executor = InterruptExecutor::<2>::new(sw_int);
    // let executor = EXECUTOR.init(executor);

    // static EXECUTOR: StaticCell<InterruptExecutor<2>> = StaticCell::new();
    // let executor =
    //     InterruptExecutor::<2>::new(system.software_interrupt_control.software_interrupt2);
    // let mut executor = EXECUTOR.init(executor);

    // executor
    // .start(Priority::Priority2)
    spawner
        .spawn(touch_task(
            touch_irq,
            touch_spi,
            touch_signal,
        ))
        .unwrap();

    // Display setup
    let sclk = io.pins.gpio14;
    let miso = io.pins.gpio12;
    let mosi = io.pins.gpio13;
    let cs = io.pins.gpio15;
    let dc = io.pins.gpio2;
    let mut backlight =
        Output::new(io.pins.gpio21, Level::Low);

    let mut spi = Spi::new(
        peripherals.SPI2,
        10u32.MHz(),
        SpiMode::Mode0,
        &clocks,
    )
    .with_pins(
        Some(sclk),
        Some(mosi),
        Some(miso),
        esp_hal::gpio::NO_PIN,
    );

    // static DISP_SPI_BUS:  StaticCell<NoopMutex<RefCell<Spi<SPI2, FullDuplexMode>>>> = StaticCell::new();
    // let spi_bus = NoopMutex::new(RefCell::new(spi));
    // let spi_bus = DISP_SPI_BUS.init(spi_bus);

    static SPI_DELAY_STATICCELL: StaticCell<Delay> =
        StaticCell::new();
    let mut delay = SPI_DELAY_STATICCELL.init(Delay);
    let spi = ExclusiveDevice::new(
        spi,
        Output::new(cs, Level::Low),
        delay,
    )
    .unwrap();

    let di =
        SPIInterface::new(spi, Output::new(dc, Level::Low));

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

    backlight.set_high();

    let size = display.bounding_box().size;
    // let size = slint::PhysicalSize::new(size.width as u32, size.height as u32);

    let window =
        MinimalSoftwareWindow::new(Default::default());
    slint::platform::set_platform(Box::new(CYDPlatform {
        window: window.clone(),
    }))
    .unwrap();

    let ui = create_slint_app();

    window.set_size(slint::PhysicalSize::new(320, 240));

    let mut buffer_provider = DrawBuffer {
        display,
        buffer: &mut [slint::platform::software_renderer::Rgb565Pixel(0); 320],
    };

    let mut last_touch = None;

    let mut appdata = Rc::new(RefCell::new(AppData::new()));

    let mut cl_appdata = appdata.clone();
    ui.on_add_10s(move || {
        cl_appdata.borrow_mut().add_secs(10)
    });
    let mut cl_appdata = appdata.clone();
    ui.on_sub_10s(move || {
        cl_appdata.borrow_mut().sub_secs(10)
    });
    let mut cl_appdata = appdata.clone();
    ui.on_start_timer(move || {
        cl_appdata.borrow_mut().start_timer()
    });
    let mut cl_appdata = appdata.clone();
    ui.on_stop_timer(move || {
        cl_appdata.borrow_mut().pause_timer()
    });
    let mut cl_appdata = appdata.clone();
    ui.on_reset_timer(move || {
        cl_appdata.borrow_mut().reset_timer()
    });

    loop {
        let start_time = Instant::now();
        if let Some(touch) = touch_signal.try_take() {
            // println!("touch: {:?}, last_touch: {:?}", touch, last_touch);
            let button = PointerEventButton::Left;
            let interact = match (touch, last_touch) {
                (Some(point), Some(_)) => {
                    Some(WindowEvent::PointerMoved {
                        position: point_to_logical_pos(
                            point,
                        ),
                    })
                }
                (Some(point), None) => {
                    Some(WindowEvent::PointerPressed {
                        position: point_to_logical_pos(
                            point,
                        ),
                        button,
                    })
                }
                (None, Some(point)) => {
                    Some(WindowEvent::PointerReleased {
                        position: point_to_logical_pos(
                            point,
                        ),
                        button,
                    })
                }
                (None, None) => None,
            };
            if let Some(event) = interact {
                // println!("event: {:?}", event);
                window.dispatch_event(event);
            }

            last_touch = touch;
        }

        {
            let appdata = appdata.borrow();
            let remaining = appdata.remaining();
            ui.set_show_reset_timer(
                appdata.timer_finished(),
            );
            ui.set_show_start_timer(
                !appdata.timer_running()
                    || appdata.timer_paused()
                        && !appdata.timer_finished(),
            );
            ui.set_show_stop_timer(
                appdata.timer_running()
                    && !appdata.timer_finished(),
            );

            ui.set_timer_text(format!(
                "{:02}:{:02}:{:03}",
                remaining.as_secs() / 60,
                remaining.as_secs() % 60,
                remaining.as_millis() % 1000
            ));
        }

        slint::platform::update_timers_and_animations();

        // let window = window.clone();
        let start_draw_time = Instant::now();
        window.draw_if_needed(|renderer| {
            // println!("dirty reg: {:?}", renderer.render_by_line(&mut buffer_provider));
            renderer.render_by_line(&mut buffer_provider);
        });

        if window.has_active_animations() {
            continue;
        }

        let display = &mut buffer_provider.display;

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
                (draw_time + prep_time + proc_time).as_micros() % 100,            );
        }
        display.reset_time();
        Timer::after(Duration::from_millis(1)).await; // 60 a second
    }
}

fn create_slint_app() -> MicrowaveUI {
    let ui = MicrowaveUI::new().unwrap();

    /*
    let ui_handle = ui.as_weak();
    ui.on_request_increase_value(move || {
        let ui = ui_handle.unwrap();
        ui.set_counter(ui.get_counter() + 1);
    });
     */

    ui
}

struct DrawBuffer<'a, DT> {
    display: DT,
    buffer: &'a mut [slint::platform::software_renderer::Rgb565Pixel],
}

impl<
        // DI: display_interface_spi::WriteOnlyDataCommand,
        E: core::fmt::Debug,
        DT: DrawTarget<Color = Rgb565, Error = E>,
        // RST: OutputPin<Error = core::convert::Infallible>,
    > slint::platform::software_renderer::LineBufferProvider
    for &mut DrawBuffer<'_, DT>
{
    type TargetPixel =
        slint::platform::software_renderer::Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: core::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [slint::platform::software_renderer::Rgb565Pixel]),
    ) {
        let buffer = &mut self.buffer[range.clone()];

        render_fn(buffer);

        // We send empty data just to get the device in the right window
        self.display
            .fill_contiguous(
                &Rectangle::new(
                    Point::new(range.start as i32, line as i32),
                    Size::new((range.end - range.start) as u32, 1),
                ),
                // range.start as u16,
                // line as _,
                // range.end as u16,
                // line as u16,
                buffer
                    .iter()
                    .map(|x| embedded_graphics_core::pixelcolor::raw::RawU16::new(x.0).into()),
            )
            .unwrap();
    }
}
