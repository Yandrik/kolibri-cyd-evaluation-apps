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
    widgets::{Btn, Label, Slider, Switch},
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

#[derive(Debug, Clone)]
struct Lamp {
    pub name: heapless::String<64>,
    pub on: bool,
    pub brightness: u8,
}

impl Lamp {
    pub fn new(name: &str) -> Self {
        Self {
            name: heapless::String::from(
                heapless::String::from_str(name).unwrap(),
            ),
            on: false,
            brightness: 255,
        }
    }
}

enum Page<'a> {
    Home,
    LampCtrl(&'a Lamp),
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
            screen_style.set_layout(Layout::flex());
            screen_style.set_flex_flow(FlexFlow::ROW_WRAP);
            screen_style.set_flex_main_place(FlexAlign::CENTER);
            screen_style.set_flex_cross_place(FlexAlign::CENTER);
            screen.add_style(Part::Main, &mut screen_style);


            appdata.add_lamp("Front Door");
            appdata.add_lamp("Living Room");
            appdata.add_lamp("Bedroom");
            appdata.add_lamp("Bathroom");
            appdata.add_lamp("Porch");

            let mut light_page = Screen::blank().unwrap();
            light_page.set_size(320, 240);
            let mut light_page_style = Style::default();
            light_page_style.set_bg_color(Color::from_rgb((0, 0, 0)));
            light_page_style.set_radius(0);
            light_page.add_style(Part::Main, &mut light_page_style);

            let mut light_page_title_label = Label::create(&mut light_page).unwrap();
            let mut light_page_title_style = Style::default();
            light_page_title_style.set_text_color(Color::from_rgb((255, 255, 255)));
            unsafe { light_page_title_style.set_text_font(Font::new_raw(lvgl_sys::lv_font_montserrat_24)) };
            light_page_title_label.add_style(Part::Main, &mut light_page_title_style);
            light_page_title_label.set_align(Align::TopMid, 0, 6);


            let mut brightness_slider = Slider::create(&mut light_page).unwrap();
            brightness_slider.set_size(250, 10);
            // brightness_slider.range(0..255);
            brightness_slider.set_value(0, AnimationState::OFF);
            brightness_slider.set_align(Align::Center, 0, -20);

            let mut brightness_slider_label = Label::create(&mut light_page).unwrap();
            // no text for now
            let mut white_text_style = Style::default();
            white_text_style.set_text_color(Color::from_rgb((255, 255, 255)));
            unsafe { white_text_style.set_text_font(Font::new_raw(lvgl_sys::lv_font_montserrat_14)) };
            brightness_slider_label.add_style(Part::Main, &mut white_text_style);
            brightness_slider_label.set_text(CString::new("Brightness").unwrap().as_c_str());
            brightness_slider_label.set_align(Align::Center, 0, -50);


            // Add switch
            let mut light_switch = Switch::create(&mut light_page).unwrap();
            light_switch.set_align(Align::Center, 0, 40);

            let mut light_switch_label = Label::create(&mut light_page).unwrap();
            // no text for now
            let mut lslabel_style = Style::default();
            lslabel_style.set_text_color(Color::from_rgb((255, 255, 255)));
            unsafe { lslabel_style.set_text_font(Font::new_raw(lvgl_sys::lv_font_montserrat_14)) };
            light_switch_label.add_style(Part::Main, &mut lslabel_style);
            light_switch_label.set_text(CString::new("On/Off").unwrap().as_c_str());
            light_switch_label.set_align(Align::Center, 0, 10);

            // Add back button
            let mut back_btn = Btn::create(&mut light_page).unwrap();
            back_btn.set_size(40, 40);
            back_btn.set_align(Align::TopLeft, 10, 10);
            back_btn.on_event(|_btn, event| {
                if let Event::Pressed = event {
                    display.set_scr_act(&mut screen);
                }
            });

            let mut back_label = Label::create(&mut back_btn).unwrap();
            back_label.set_text(CString::new(b"\xef\x81\x93").unwrap().as_c_str()); // Font Awesome left arrow
            let mut back_style = Style::default();
            unsafe { back_style.set_text_font(Font::new_raw(lvgl_sys::lv_font_montserrat_24)) };
            back_label.add_style(Part::Main, &mut back_style);

            let mut light_ctrls = heapless::Vec::<(Btn, Label, Label), 8>::new();

            let mut page = Page::Home;

            for lamp in appdata.lamps.iter_mut() {
                let mut cont = Btn::new().unwrap();
                cont.set_size(100, 110);
                // cont.set_align(Align::TopLeft, 0, 0);

                cont.on_event(|_btn, event| {
                    if let Event::Pressed = event {
                        // println!("lamp {:?}", lamp.name);
                        // page = Page::LampCtrl(lamp);

                        brightness_slider.set_value(lerp_fixed(0, 100, lamp.brightness, 255).into(), AnimationState::OFF);
                        unsafe {
                            if lamp.on {
                                lvgl_sys::lv_obj_add_state(light_switch.raw().as_ptr(), lvgl_sys::LV_STATE_CHECKED as u16);
                            } else {
                                lvgl_sys::lv_obj_clear_state(light_switch.raw().as_ptr(), lvgl_sys::LV_STATE_CHECKED as u16);
                            }
                        }
                        light_switch.on_event(|_ls, event| {
                            if let Event::ValueChanged | Event::Released = event {
                                let on = unsafe { lvgl_sys::lv_obj_has_state(_ls.raw().as_ptr(), lvgl_sys::LV_STATE_CHECKED as u16) };
                                lamp.on = on;
                            }
                        });
                        brightness_slider.on_event(|_sldr, event| {
                            // println!("event: {:?}", event);
                            if let Event::ValueChanged | Event::Released = event {
                                let brightness = _sldr.get_value();
                                println!("brightness: {}", brightness);
                                lamp.brightness = lerp_fixed(0, 255, brightness as u8, 100);
                                println!("brightness: {}", lamp.brightness);
                            }
                        });
                        light_page_title_label.set_text(CString::new(lamp.name.as_str()).unwrap().as_c_str());

                        display.set_scr_act(&mut light_page);
                    }
                });

                let mut style_name = Style::default();
                style_name.set_text_align(TextAlign::Center);
                style_name.set_text_color(Color::from_rgb((255, 255, 255)));
                unsafe { style_name.set_text_font(Font::new_raw(lvgl_sys::lv_font_montserrat_14)) };

                let mut style_icon = Style::default();
                style_icon.set_text_color(Color::from_rgb((255, 255, 255)));
                unsafe { style_icon.set_text_font(Font::new_raw(lvgl_sys::lv_font_montserrat_32)) };
                style_icon.set_text_align(TextAlign::Center);


                let mut icon = Label::create(&mut cont).unwrap();
                icon.set_text(CString::new(b"\xef\x84\xa4").unwrap().as_c_str());
                icon.add_style(Part::Main, Box::leak(Box::new(style_icon)));
                icon.set_align(Align::Center, 0, 0);

                let mut name = Label::create(&mut cont).unwrap();
                name.set_text(CString::new(lamp.name.as_str()).unwrap().as_c_str());
                // style_name.set_text_color(Color::from_rgb((255, 255, 255))); // white
                name.add_style(Part::Main, Box::leak(Box::new(style_name)));
                name.set_align(Align::BottomMid, 0, -4);

                light_ctrls.push((cont, icon, name))
                    .unwrap();
            }


            // let mut time = Label::new().unwrap();
            // time.set_text(CString::new("00:10:000").unwrap().as_c_str());
            // let mut style_time = Style::default();
            // style_time.set_text_color(Color::from_rgb((255, 255, 255))); // white
            // style_time.set_text_align(TextAlign::Center);
            // unsafe { style_time.set_text_font(Font::new_raw(lvgl_sys::lv_font_montserrat_24)) };
            // time.add_style(Part::Main, &mut style_time);
            // time.set_align(Align::Center, 0, 0);

            /*


            // let mut cont = Obj::new().unwrap();
            // cont.set_size(320, 50);
            // let mut style = Style::default();
            // style.set_flex_flow(FlexFlow::ROW);
            // style.set_flex_main_place(FlexAlign::SPACE_EVENLY);
            // style.set_flex_cross_place(FlexAlign::CENTER);
            // // style.set_bg_opa(0);
            // style.set_border_width(0);
            // cont.add_style(Part::Any, &mut style);
            // cont.set_align(Align::Center, 0, 40);

            let mut button_add = Btn::create(&mut screen).unwrap();
            button_add.set_align(Align::Center, -50, 32);
            // button_add.set_pos(130, 150);
            button_add.set_size(30, 30);
            // button_add.set_align(Align::Center, 0, 0);
            let mut btn_lbl1 = Label::create(&mut button_add).unwrap();
            btn_lbl1.set_text(CString::new(b"+").unwrap().as_c_str());

            button_add.on_event(|_btn, event| {
                if let Event::Pressed = event {
                    println!("pressed");
                    if appdata.timer_stopped() {
                        appdata.add_secs(10);
                    }
                }
                // println!("Button received event: {:?}", event);
            });

            let mut button_sub = Btn::create(&mut screen).unwrap();
            // button_sub.set_pos(170, 150);
            button_sub.set_size(30, 30);
            button_sub.set_align(Align::Center, 50, 35);
            // button_sub.set_align(Align::Center, 0, 0);
            let mut btn_lbl2 = Label::create(&mut button_sub).unwrap();
            btn_lbl2.set_text(CString::new(b"-").unwrap().as_c_str());

            button_sub.on_event(|_btn, event| {
                if let Event::Pressed = event {
                    if appdata.timer_stopped() {
                        appdata.sub_secs(10);
                    }
                }
            });

            let mut center_label = Label::new().unwrap();
            center_label.set_text(CString::new(b"+/- 10s").unwrap().as_c_str());
            center_label.set_align(Align::Center, 0, 35);
            let mut cl_style = Style::default();
            cl_style.set_text_color(Color::from_rgb((255, 255, 255)));
            center_label.add_style(Part::Main, &mut cl_style);

            let mut button_reset = Btn::create(&mut screen).unwrap();
            //button_reset.set_pos(123, 190);
            button_reset.set_align(Align::Center, -22, 70);
            button_reset.set_size(35, 35);
            let mut btn_lbl3 = Label::create(&mut button_reset).unwrap();
            btn_lbl3.set_text(CString::new(b"\xEF\x80\xA1").unwrap().as_c_str());


            const PLAY: &'static [u8; 3] = b"\xEF\x81\x8B";
            const PAUSE: &'static [u8; 3] = b"\xEF\x81\x8C";
            const STOP: &'static [u8; 3] = b"\xEF\x81\x8D";

            let mut button_start_stop = Btn::create(&mut screen).unwrap();
            // button_start_stop.set_pos(173, 190);
            button_start_stop.set_align(Align::Center, 22, 70);
            button_start_stop.set_size(35, 35);
            let mut btn_lbl4 = Label::create(&mut button_start_stop).unwrap();
            btn_lbl4.set_text(CString::new(b"\xEF\x81\x8B").unwrap().as_c_str());

            button_reset.on_event(|_btn, event| {
                if let Event::Pressed = event {
                    appdata.reset_timer();
                    btn_lbl4.set_text(CString::new(PLAY).unwrap().as_c_str());
                }
            });

            button_start_stop.on_event(|_btn, event| {
                if let Event::Pressed = event {
                    if appdata.timer_finished() {
                        // println!("Resetting finished timer");
                        appdata.reset_timer();
                        btn_lbl4.set_text(CString::new(PLAY).unwrap().as_c_str());
                    } else if appdata.timer_running() {
                        // println!("Pausing timer");
                        appdata.pause_timer();
                        btn_lbl4.set_text(CString::new(PLAY).unwrap().as_c_str());
                    } else {
                        // println!("Starting timer");
                        appdata.start_timer();
                        if appdata.timer_finished() {
                            // println!("Timer already finished");
                            btn_lbl4.set_text(CString::new(STOP).unwrap().as_c_str());
                        } else {
                            // println!("Timer running");
                            btn_lbl4.set_text(CString::new(PAUSE).unwrap().as_c_str());
                        }
                    }
                }
            });

            let mut was_finished = false;
            let mut last_rem_time: Duration = appdata.remaining() + Duration::from_millis(10);

             */

            let mut last_time = Instant::now();
            loop {
                let start_time = Instant::now();

                let start_draw_time = Instant::now();
                lvgl::task_handler();

                // Simulate clock - so sleep for one second so time text is incremented in
                // seconds
                // delay::FreeRtos::delay_ms(1);

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
