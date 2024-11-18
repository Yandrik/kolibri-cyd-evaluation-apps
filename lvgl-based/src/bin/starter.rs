use std::{
    cell::RefCell,
    sync::mpsc::channel,
    thread,
    time::{Duration, Instant},
};
use std::cmp::min;
use cstr_core::CString;
use display_interface_spi::SPIInterface;
use embedded_graphics_core::{draw_target::DrawTarget, prelude::Point};
use esp_idf_hal::spi::SpiSingleDeviceDriver;
use esp_idf_hal::{
    delay::{self, Delay},
    gpio::*,
    peripherals::Peripherals,
    spi::{config::DriverConfig, Dma, SpiConfig, SpiDeviceDriver},
    units::FromValueType, // for converting 26MHz to value
};
use lvgl::{
    font::Font,
    input_device::{
        pointer::{Pointer, PointerInputData},
        InputDriver,
    },
    style::Style,
    widgets::{Btn, Label},
    Align,
    Color,
    Display,
    DrawBuffer,
    LvError,
    Part,
    TextAlign,
    Widget,
};
use mipidsi::{
    models::ILI9486Rgb565,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    Builder,
};
use xpt2046::Xpt2046;
use embedded_graphics_profiler_display::ProfilerDisplay;

fn main() -> Result<(), LvError> {
    const HOR_RES: u32 = 320;
    const VER_RES: u32 = 240;
    const LINES: u32 = 20;

    // It is necessary to call this function once. Otherwise some patches to the
    // runtime implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Initialize lvgl
    lvgl::init();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("================= Starting App =======================");

    let peripherals = Peripherals::take().unwrap();

    // #[allow(unused)]
    let pins = peripherals.pins;

    let sclk = pins.gpio14;
    let miso = pins.gpio12;
    let mosi = pins.gpio13;
    let cs = pins.gpio15;
    let dc = pins.gpio2;

    let spi = SpiDeviceDriver::new_single(
        peripherals.spi2,
        sclk,       // sclk
        mosi,       // sdo
        Some(miso), // sdi
        Some(cs),   // cs
        &DriverConfig::new().dma(Dma::Channel1(4096)),
        &SpiConfig::new().write_only(true).baudrate(10.MHz().into()),
    )
    .unwrap();

    // let rst = PinDriver::output(pins.gpio33).unwrap();
    let dc = PinDriver::output(dc).unwrap();
    let di = SPIInterface::new(spi, dc);

    // Turn backlight on
    let mut bklt = PinDriver::output(pins.gpio21).unwrap();
    bklt.set_high().unwrap();

    // Configuration for M5Stack Core Development Kit V1.0
    // Puts display in landscape mode with the three buttons at the bottom of screen
    // let mut m5stack_display = Builder::ili9342c_rgb565(di)
    //     .with_display_size(320, 240)
    //     .with_color_order(ColorOrder::Bgr)
    //     .with_orientation(Orientation::Portrait(false))
    //     .with_invert_colors(mipidsi::ColorInversion::Inverted)
    //     .init(&mut delay::Ets, Some(rst))
    //     .unwrap();

    // 0.6.0
    // let mut raw_display = Builder::ili9342c_rgb565(di)
    //     .with_orientation(Orientation::Portrait(false))
    //     .with_color_order(ColorOrder::Bgr)
    //     .with_invert_colors(true)
    //     .init(&mut Delay::new_default(), None::<PinDriver<AnyOutputPin, Output>>)
    //     .unwrap();

    let raw_display = Builder::new(ILI9486Rgb565, di)
        .orientation(Orientation {
            rotation: Rotation::Deg90,
            mirrored: true,
        })
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay::new_default())
        .unwrap();

    let mut raw_display = ProfilerDisplay::new(raw_display);

    // Stack size value - 20,000 for 10 lines,  40,000 for 20 lines
    // let (touch_send, touch_recv) = channel();
    let touch_irq = pins.gpio36;
    let touch_mosi = pins.gpio32;
    let touch_miso = pins.gpio39;
    let touch_clk = pins.gpio25;
    let touch_cs = pins.gpio33;

    let mut touch_driver = Xpt2046::new(
        SpiDeviceDriver::new_single(
            peripherals.spi3,
            touch_clk,
            touch_mosi,
            Some(touch_miso),
            Some(touch_cs),
            &DriverConfig::new(),
            &SpiConfig::new().write_only(true).baudrate(2.MHz().into()),
        )
        .unwrap(),
        PinDriver::input(touch_irq).unwrap(),
        xpt2046::Orientation::LandscapeFlipped,
    );
    touch_driver.set_num_samples(1);
    touch_driver.init(&mut Delay::new_default()).unwrap();

    let touch_driver = RefCell::new(touch_driver);

    let _lvgl_thread = thread::Builder::new()
        .stack_size(40_000)
        .spawn(move || {
            println!("thread started");

            let buffer = DrawBuffer::<{ (HOR_RES * LINES) as usize }>::default();
            let display = Display::register(buffer, HOR_RES, VER_RES, |refresh| {
                raw_display.draw_iter(refresh.as_pixels()).unwrap();
            })
            .unwrap();

            // Register a new input device that's capable of reading the current state of
            // the input
            // let mut last_touch = RefCell::new(None);
            let _touch_screen = Pointer::register(
                || {
                    let mut td_ref = touch_driver.borrow_mut();
                    td_ref.run().expect("Running Touch driver failed");
                    if td_ref.is_touched() {
                        let point = td_ref.get_touch_point();
                        PointerInputData::Touch(point).pressed().once()
                    } else {
                        PointerInputData::Touch(Point::new(0, 0)).released().once()
                    }
                },
                &display,
            );

            // Create screen and widgets
            let mut screen = display.get_scr_act().unwrap();
            let mut screen_style = Style::default();
            screen_style.set_bg_color(Color::from_rgb((0, 0, 0)));
            screen_style.set_radius(0);
            screen.add_style(Part::Main, &mut screen_style);

            let mut time = Label::new().unwrap();
            let mut style_time = Style::default();
            style_time.set_text_color(Color::from_rgb((255, 255, 255))); // white
            style_time.set_text_align(TextAlign::Center);

            // Custom font requires lvgl-sys in Cargo.toml and 'use lvgl_sys' in this file
            style_time.set_text_font(unsafe { Font::new_raw(lvgl_sys::gotham_bold_80) });

            time.add_style(Part::Main, &mut style_time);

            // Time text will be centered in screen
            time.set_align(Align::Center, 0, 0);

            let mut button = Btn::create(&mut screen).unwrap();
            button.set_align(Align::LeftMid, 30, 0);
            button.set_size(180, 80);
            let mut btn_lbl = Label::create(&mut button).unwrap();
            btn_lbl.set_text(CString::new("Click me!").unwrap().as_c_str());

            let mut btn_state = false;
            button.on_event(|_btn, event| {
                // println!("Button received event: {:?}", event);
                if let lvgl::Event::Clicked = event {
                    if btn_state {
                        let nt = CString::new("Click me!").unwrap();
                        btn_lbl.set_text(nt.as_c_str()).unwrap();
                    } else {
                        let nt = CString::new("Clicked!").unwrap();
                        btn_lbl.set_text(nt.as_c_str()).unwrap();
                    }
                    btn_state = !btn_state;
                }
            });

            let mut i = 0;

            loop {
                let start_time = Instant::now();
                if i > 59 {
                    i = 0;
                }

                let val = CString::new(format!("21:{:02}", i)).unwrap();
                time.set_text(&val).unwrap();
                i += 1;

                let start_draw_time = Instant::now();
                lvgl::task_handler();

                // Simulate clock - so sleep for one second so time text is incremented in
                // seconds
                delay::FreeRtos::delay_ms(10);

                lvgl::tick_inc(Instant::now().duration_since(start_time));


                let end_time = Instant::now();
                let draw_time = raw_display.get_time();
                let prep_time = start_draw_time - start_time;
                let proc_time = end_time - start_draw_time;
                let proc_time = proc_time - min(draw_time, proc_time);

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
                raw_display.reset_time();
            }
        })
        .unwrap();

    loop {
        // Don't exit application
        delay::FreeRtos::delay_ms(1_000_000);
    }
}
