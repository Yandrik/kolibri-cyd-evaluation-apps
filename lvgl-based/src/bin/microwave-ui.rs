use std::{
    cell::RefCell,
    cmp::min,
    str::FromStr,
    sync::mpsc::channel,
    thread,
    time::{Duration, Instant},
};

use cstr_core::CString;
use display_interface_spi::SPIInterface;
use embedded_graphics_core::{
    draw_target::DrawTarget,
    prelude::Point,
};
use embedded_graphics_profiler_display::ProfilerDisplay;
use esp_idf_hal::spi::SpiSingleDeviceDriver;
use esp_idf_hal::{
    delay::{self, Delay},
    gpio::*,
    peripherals::Peripherals,
    spi::{
        config::DriverConfig,
        Dma,
        SpiConfig,
        SpiDeviceDriver,
    },
    units::FromValueType, // for converting 26MHz to value
};
use lvgl::{
    font::Font,
    input_device::{
        pointer::{Pointer, PointerInputData},
        InputDriver,
    },
    style::{FlexAlign, FlexFlow, Layout, Style},
    widgets::{Arc, Btn, Label, Slider, Switch},
    Align,
    AnimationState,
    Color,
    Display,
    DrawBuffer,
    Event,
    LvError,
    NativeObject,
    Obj,
    Part,
    Screen,
    TextAlign,
    Widget,
};
use mipidsi::{
    models::ILI9341Rgb565,
    options::{
        ColorInversion,
        ColorOrder,
        Orientation,
        Rotation,
    },
    Builder,
};
use xpt2046::Xpt2046;

fn lerp_fixed(start: u8, end: u8, t: u8, max_t: u8) -> u8 {
    let (start, end, t, max_t) =
        (start as u16, end as u16, t as u16, max_t as u16);
    let t = t.min(max_t);
    let result = start
        + ((end - start.min(end)) * t + (max_t / 2))
            / max_t;
    result as u8
}
struct AppData {
    timer_start: Instant,
    timer_set_duration: Duration,
    timer_remaining_duration: Duration,
    timer_running: bool,
    timer_paused: bool,
    wattage_level: u8,
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
            wattage_level: 5,
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

    fn set_wattage_level(&mut self, level: u8) {
        assert!(
            level < 6,
            "wattage level cannot be over 6"
        );
        self.wattage_level = level;
    }

    fn get_wattage_level_str(&self) -> &'static str {
        match self.wattage_level {
            0 => "180W",
            1 => "220W",
            2 => "360W",
            3 => "480W",
            4 => "620W",
            5 => "800W",
            _ => unreachable!(),
        }
    }
}

fn main() -> Result<(), LvError> {
    const HOR_RES: u32 = 320;
    const VER_RES: u32 = 240;
    const LINES: u32 = 20;

    // It is necessary to call this function once. Otherwise
    // some patches to the runtime implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
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
        &SpiConfig::new()
            .write_only(true)
            .baudrate(10.MHz().into()),
    )
    .unwrap();

    // let rst = PinDriver::output(pins.gpio33).unwrap();
    let dc = PinDriver::output(dc).unwrap();
    let di = SPIInterface::new(spi, dc);

    // Turn backlight on
    let mut bklt = PinDriver::output(pins.gpio21).unwrap();
    bklt.set_high().unwrap();

    // Configuration for M5Stack Core Development Kit V1.0
    // Puts display in landscape mode with the three buttons
    // at the bottom of screen let mut m5stack_display =
    // Builder::ili9342c_rgb565(di)
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
    //     .init(&mut Delay::new_default(),
    // None::<PinDriver<AnyOutputPin, Output>>)
    //     .unwrap();

    let raw_display = Builder::new(ILI9341Rgb565, di)
        .orientation(Orientation {
            rotation: Rotation::Deg90,
            mirrored: true,
        })
        .color_order(ColorOrder::Bgr)
        // .invert_colors(ColorInversion::Inverted)
        .init(&mut Delay::new_default())
        .unwrap();

    let mut raw_display = ProfilerDisplay::new(raw_display);

    // Stack size value - 20,000 for 10 lines,  40,000 for
    // 20 lines let (touch_send, touch_recv) =
    // channel();
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
            &SpiConfig::new()
                .write_only(true)
                .baudrate(2.MHz().into()),
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

            let mut appdata = AppData::new();

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
                        // println!("touched {:?}", point);
                        PointerInputData::Touch(Point::new(point.x + 20, 240 - point.y)).pressed().once()
                    } else {
                        // println!("untouched");
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
            // screen_style.set_layout(Layout::flex());
            // screen_style.set_flex_flow(FlexFlow::ROW_WRAP);
            // screen_style.set_flex_main_place(FlexAlign::CENTER);
            // screen_style.set_flex_cross_place(FlexAlign::CENTER);
            screen.add_style(Part::Main, &mut screen_style);


            let mut wattage_label = Label::create(&mut screen).unwrap();
            wattage_label.set_align(Align::Center, -70, 0);
            let mut wattage_style = Style::default();
            wattage_style.set_text_color(Color::from_rgb((255, 255, 255)));
            unsafe { wattage_style.set_text_font(Font::new_raw(lvgl_sys::lv_font_montserrat_32)) };
            wattage_label.add_style(Part::Main, &mut wattage_style);
            wattage_label.set_text(CString::new(appdata.get_wattage_level_str()).unwrap().as_c_str());


            let mut power_arc = Arc::create(&mut screen).unwrap();
            power_arc.set_size(150, 150);
            power_arc.set_align(Align::Center, -70, 0);
            power_arc.set_angles(135, 45); // Creates a 270-degree arc
            power_arc.set_rotation(0);
            power_arc.set_bg_angles(135, 45);
            unsafe {
                lvgl_sys::lv_arc_set_range(power_arc.raw().as_mut(), 0, 5);
            }

            power_arc.on_event(|arc, event| {
                if let Event::ValueChanged = event {
                    let mut value = unsafe {
                        lvgl_sys::lv_arc_get_value(arc.raw().as_mut())
                    };
                    appdata.set_wattage_level((value as u8).min(6));
                    wattage_label.set_text(CString::new(appdata.get_wattage_level_str()).unwrap().as_c_str());
                }
            });

            // Add timer display on the right
            let mut time_label = Label::new().unwrap();
            time_label.set_text(CString::new("00:10:000").unwrap().as_c_str());
            let mut style_time = Style::default();
            style_time.set_text_color(Color::from_rgb((255, 255, 255)));
            style_time.set_text_align(TextAlign::Center);
            unsafe { style_time.set_text_font(Font::new_raw(lvgl_sys::lv_font_montserrat_24)) };
            time_label.add_style(Part::Main, &mut style_time);
            time_label.set_align(Align::Center, 80, 0);

            // Add + and - buttons at bottom
            let mut button_add = Btn::create(&mut screen).unwrap();
            button_add.set_align(Align::Center, 10, 70);
            button_add.set_size(30, 30);
            let mut btn_lbl1 = Label::create(&mut button_add).unwrap();
            btn_lbl1.set_text(CString::new(b"+").unwrap().as_c_str());

            let mut button_sub = Btn::create(&mut screen).unwrap();
            button_sub.set_size(30, 30);
            button_sub.set_align(Align::Center, 130, 70);
            let mut btn_lbl2 = Label::create(&mut button_sub).unwrap();
            btn_lbl2.set_text(CString::new(b"-").unwrap().as_c_str());

            // Add timer control logic from timer.rs
            button_add.on_event(|_btn, event| {
                if let Event::Pressed = event {
                    if appdata.timer_stopped() {
                        appdata.add_secs(10);
                    }
                }
            });

            button_sub.on_event(|_btn, event| {
                if let Event::Pressed = event {
                    if appdata.timer_stopped() {
                        appdata.sub_secs(10);
                    }
                }
            });

            // Add reset button
            let mut button_reset = Btn::create(&mut screen).unwrap();
            button_reset.set_align(Align::Center, 50, 70);
            button_reset.set_size(35, 35);
            let mut btn_lbl3 = Label::create(&mut button_reset).unwrap();
            btn_lbl3.set_text(CString::new(b"\xEF\x80\xA1").unwrap().as_c_str());

            // Add start/stop/pause button
            const PLAY: &'static [u8; 3] = b"\xEF\x81\x8B";
            const PAUSE: &'static [u8; 3] = b"\xEF\x81\x8C";
            const STOP: &'static [u8; 3] = b"\xEF\x81\x8D";

            let mut button_start_stop = Btn::create(&mut screen).unwrap();
            button_start_stop.set_align(Align::Center, 90, 70);
            button_start_stop.set_size(35, 35);
            let mut btn_lbl4 = Label::create(&mut button_start_stop).unwrap();
            btn_lbl4.set_text(CString::new(PLAY).unwrap().as_c_str());

            // Add event handlers
            button_reset.on_event(|_btn, event| {
                if let Event::Pressed = event {
                    appdata.reset_timer();
                    btn_lbl4.set_text(CString::new(PLAY).unwrap().as_c_str());
                    unsafe {
                        lvgl_sys::lv_obj_clear_state(button_add.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                        lvgl_sys::lv_obj_clear_state(button_sub.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                        lvgl_sys::lv_obj_clear_state(power_arc.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                    }
                }
            });

            button_start_stop.on_event(|_btn, event| {
                if let Event::Pressed = event {
                    if appdata.timer_finished() {
                        appdata.reset_timer();
                        btn_lbl4.set_text(CString::new(PLAY).unwrap().as_c_str());
                        // Enable all buttons
                        unsafe {
                            lvgl_sys::lv_obj_clear_state(button_add.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                            lvgl_sys::lv_obj_clear_state(button_sub.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                            lvgl_sys::lv_obj_clear_state(power_arc.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                        }
                    } else if appdata.timer_running() {
                        appdata.pause_timer();
                        btn_lbl4.set_text(CString::new(PLAY).unwrap().as_c_str());
                        // Enable all buttons
                        unsafe {
                            lvgl_sys::lv_obj_clear_state(button_add.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                            lvgl_sys::lv_obj_clear_state(button_sub.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                            lvgl_sys::lv_obj_clear_state(power_arc.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                        }
                    } else {
                        appdata.start_timer();
                        if appdata.timer_finished() {
                            btn_lbl4.set_text(CString::new(STOP).unwrap().as_c_str());
                        } else {
                            btn_lbl4.set_text(CString::new(PAUSE).unwrap().as_c_str());
                            // Disable buttons while running
                            unsafe {
                                lvgl_sys::lv_obj_add_state(button_add.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                                lvgl_sys::lv_obj_add_state(button_sub.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                                lvgl_sys::lv_obj_add_state(power_arc.raw().as_mut(), lvgl_sys::LV_STATE_DISABLED as u16);
                            }
                        }
                    }
                }
            });

            let mut was_finished = false;
            let mut last_rem_time: Duration = appdata.remaining() + Duration::from_millis(10);

            let mut last_time = Instant::now();
            loop {
                let start_time = Instant::now();

                let dur = appdata.remaining();
                if last_rem_time != dur {
                    let val = CString::new(format!("{:02}:{:02}:{:03}",
                                                    dur.as_secs() / 60,
                                                    dur.as_secs() % 60,
                                                    dur.as_millis() % 1000)).unwrap();

                    time_label.set_text(&val).unwrap();
                    last_rem_time = dur;
                }

                if !was_finished && appdata.timer_finished() {
                    btn_lbl4.set_text(CString::new(STOP).unwrap().as_c_str());
                }
                was_finished = appdata.timer_finished();

                let start_draw_time = Instant::now();
                lvgl::task_handler();

                let now_time = Instant::now();
                lvgl::tick_inc(now_time.duration_since(last_time));
                last_time = now_time;

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
                        (draw_time + prep_time + proc_time).as_micros() % 100, );
                }
                raw_display.reset_time();

                delay::FreeRtos::delay_ms(2);
            }
        })
        .unwrap();

    loop {
        // Don't exit application
        delay::FreeRtos::delay_ms(1_000_000);
    }
}
