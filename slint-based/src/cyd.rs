use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::RefCell;
use core::mem::MaybeUninit;
use display_interface_spi::SPIInterface;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use embedded_graphics_core::geometry::{OriginDimensions, Point};
use embedded_graphics_profiler_display::ProfilerDisplay;
use embedded_hal::digital::OutputPin;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::clock::ClockControl;
use esp_hal::delay::Delay;
pub use esp_hal::entry;
use esp_hal::gpio::{GpioPin, Input, Io, Level, Output, Pull, NO_PIN};
use esp_hal::interrupt::Priority;
use esp_hal::peripherals::{Peripherals, SPI3};
use esp_hal::prelude::*;
use esp_hal::rtc_cntl::Rtc;
use esp_hal::spi::{master::Spi, FullDuplexMode, SpiMode};
use esp_hal::system::SystemControl;
use esp_hal::timer::timg::TimerGroup;
use esp_hal_embassy::InterruptExecutor;
use mipidsi::models::ILI9486Rgb565;
use mipidsi::options::{ColorInversion, ColorOrder, Rotation};
use mipidsi::{options::Orientation, Builder, Display};
use xpt2046::Xpt2046;

#[embassy_executor::task]
async fn touch_task(
    touch_irq: GpioPin<36>,
    spi: ExclusiveDevice<
        Spi<'static, SPI3, FullDuplexMode>,
        Output<'static, GpioPin<33>>,
        &'static mut Delay,
    >,
    touch_signal: &'static Signal<NoopRawMutex, Option<Point>>,
) -> ! {
    let mut touch_driver = Xpt2046::new(
        spi,
        Input::new(touch_irq, Pull::Up),
        xpt2046::Orientation::LandscapeFlipped,
    );
    touch_driver.set_num_samples(16);
    touch_driver.init(&mut embassy_time::Delay).unwrap();

    esp_println::println!("touch task");

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

pub fn init() {
    const HEAP_SIZE: usize = 250 * 1024;
    static mut HEAP: MaybeUninit<[u8; HEAP_SIZE]> = MaybeUninit::uninit();
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            HEAP.as_mut_ptr() as *mut u8,
            HEAP_SIZE,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }
    slint::platform::set_platform(Box::new(EspBackend::default()))
        .expect("backend already initialized");
}

#[derive(Default)]
struct EspBackend {
    window: RefCell<Option<Rc<slint::platform::software_renderer::MinimalSoftwareWindow>>>,
}

impl slint::platform::Platform for EspBackend {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        let window = slint::platform::software_renderer::MinimalSoftwareWindow::new(
            slint::platform::software_renderer::RepaintBufferType::ReusedBuffer,
        );
        self.window.replace(Some(window.clone()));
        Ok(window)
    }

    fn duration_since_start(&self) -> core::time::Duration {
        embassy_time::Instant::from_millis(0).elapsed().into()
    }

    //noinspection DuplicatedCode
    fn run_event_loop(&self) -> Result<(), slint::PlatformError> {
        use display_interface_spi::SPIInterface;
        use embedded_hal_bus::spi::ExclusiveDevice;
        use esp_hal::delay::Delay;
        use esp_hal::gpio::{Io, Level, Output};
        use esp_hal::rtc_cntl::Rtc;
        use esp_hal::spi::{master::Spi, SpiMode};
        use esp_hal::timer::timg::TimerGroup;
        use esp_hal::{self, prelude::*};
        use mipidsi::{
            options::{ColorInversion, ColorOrder, Orientation, Rotation},
            Builder,
        };
        use static_cell::StaticCell;

        let peripherals = Peripherals::take();
        let system = SystemControl::new(peripherals.SYSTEM);
        let mut clocks = ClockControl::boot_defaults(system.clock_control).freeze();

        let mut rtc = Rtc::new(peripherals.LPWR);
        rtc.rwdt.disable();
        let mut timer_group0 = TimerGroup::new(peripherals.TIMG0, &clocks);
        timer_group0.wdt.disable();
        let mut timer_group1 = TimerGroup::new(peripherals.TIMG1, &clocks);
        timer_group1.wdt.disable();

        esp_hal_embassy::init(&clocks, timer_group0.timer0);

        let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);

        let touch_irq = io.pins.gpio36;
        let touch_mosi = io.pins.gpio32;
        let touch_miso = io.pins.gpio39;
        let touch_clk = io.pins.gpio25;
        let touch_cs = io.pins.gpio33;

        // 2MHz is the MAX! DO NOT DECREASE! This is really important.
        let mut touch_spi = Spi::new(peripherals.SPI3, 2.MHz(), SpiMode::Mode0, &mut clocks)
            .with_pins(Some(touch_clk), Some(touch_mosi), Some(touch_miso), NO_PIN);

        let touch_spi =
            ExclusiveDevice::new(touch_spi, Output::new(touch_cs, Level::Low), &mut Delay).unwrap();

        let touch_signal = Signal::new();
        static TOUCH_SIGNAL: StaticCell<Signal<NoopRawMutex, Option<Point>>> = StaticCell::new();
        let touch_signal = &*TOUCH_SIGNAL.init(touch_signal);

        let sw_int = system.software_interrupt_control.software_interrupt2;

        static EXECUTOR: StaticCell<InterruptExecutor<2>> = StaticCell::new();
        let executor = InterruptExecutor::<2>::new(sw_int);
        let executor = EXECUTOR.init(executor);


        executor.spawner().unwrap()
            .spawn(touch_task(touch_irq, touch_spi, touch_signal))
            .unwrap();

        executor.start(Priority::Priority1);

        // Display setup
        let sclk = io.pins.gpio14;
        let miso = io.pins.gpio12;
        let mosi = io.pins.gpio13;
        let cs = io.pins.gpio15;
        let dc = io.pins.gpio2;
        let mut backlight = Output::new(io.pins.gpio21, Level::Low);

        let mut spi = Spi::new(peripherals.SPI2, 10u32.MHz(), SpiMode::Mode0, &clocks).with_pins(
            Some(sclk),
            Some(mosi),
            Some(miso),
            esp_hal::gpio::NO_PIN,
        );


        // static DISP_SPI_BUS:  StaticCell<NoopMutex<RefCell<Spi<SPI2, FullDuplexMode>>>> = StaticCell::new();
        // let spi_bus = NoopMutex::new(RefCell::new(spi));
        // let spi_bus = DISP_SPI_BUS.init(spi_bus);

        let spi = ExclusiveDevice::new(spi, Output::new(cs, Level::Low), &mut Delay).unwrap();

        let di = SPIInterface::new(spi, Output::new(dc, Level::Low));

        let display = Builder::new(ILI9486Rgb565, di)
            .orientation(Orientation { rotation: Rotation::Deg90, mirrored: true })
            .color_order(ColorOrder::Bgr)
            .invert_colors(ColorInversion::Inverted)
            .init(&mut embassy_time::Delay)
            .unwrap();

        let mut display = ProfilerDisplay::new(display);

        backlight.set_high().unwrap();

        let size = display.size();
        let size = slint::PhysicalSize::new(size.width as u32, size.height as u32);

        self.window.borrow().as_ref().unwrap().set_size(size);

        let mut buffer_provider = DrawBuffer {
            display,
            buffer: &mut [slint::platform::software_renderer::Rgb565Pixel(0); 320],
        };

        loop {
            slint::platform::update_timers_and_animations();

            if let Some(window) = self.window.borrow().clone() {
                window.draw_if_needed(|renderer| {
                    renderer.render_by_line(&mut buffer_provider);
                });
                if window.has_active_animations() {
                    continue;
                }
            }
            // TODO: Implement touch handling
        }
    }

    fn debug_log(&self, arguments: core::fmt::Arguments) {
        esp_println::println!("{}", arguments);
    }
}

struct DrawBuffer<'a, Display> {
    display: Display,
    buffer: &'a mut [slint::platform::software_renderer::Rgb565Pixel],
}

impl<
        DI: display_interface_spi::WriteOnlyDataCommand,
        RST: OutputPin<Error = core::convert::Infallible>,
    > slint::platform::software_renderer::LineBufferProvider
    for &mut DrawBuffer<'_, Display<DI, mipidsi::models::ILI9342CRgb565, RST>>
{
    type TargetPixel = slint::platform::software_renderer::Rgb565Pixel;

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
            .set_pixels(
                range.start as u16,
                line as _,
                range.end as u16,
                line as u16,
                buffer
                    .iter()
                    .map(|x| embedded_graphics_core::pixelcolor::raw::RawU16::new(x.0).into()),
            )
            .unwrap();
    }
}
