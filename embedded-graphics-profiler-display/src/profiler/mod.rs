use atomic::Atomic;
use embassy_time::{Duration, Instant};
use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::{Drawable, Pixel};
use embedded_graphics::geometry::Dimensions;
use embedded_graphics::primitives::Rectangle;

pub struct ProfilerDisplay<DRAW_TARGET: DrawTarget> {
    drawtarget: DRAW_TARGET,
    time_draw: Duration,
    time_draw_iter: Duration,
    time_fill_contiguous: Duration,
    time_fill_solid: Duration,
}

impl<DRAW_TARGET: DrawTarget> Dimensions for ProfilerDisplay<DRAW_TARGET> {
    fn bounding_box(&self) -> Rectangle {
        self.drawtarget.bounding_box()
    }
}

impl<DRAW_TARGET: DrawTarget> DrawTarget for ProfilerDisplay<DRAW_TARGET> {
    type Color = DRAW_TARGET::Color;
    type Error = DRAW_TARGET::Error;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item=Pixel<Self::Color>>
    {
        let start = Instant::now();
        let res = self.drawtarget.draw_iter(pixels);
        self.time_draw_iter += Instant::now() - start;
        res
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item=Self::Color>
    {
        let start = Instant::now();
        let res = self.drawtarget.fill_contiguous(area, colors);
        self.time_fill_contiguous += Instant::now() - start;
        res
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let start = Instant::now();
        let res = self.drawtarget.fill_solid(area, color);
        self.time_fill_solid += Instant::now() - start;
        res
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        let start = Instant::now();
        let res = self.drawtarget.clear(color);
        self.time_fill_solid += Instant::now() - start;
        res
    }
}

impl<DRAW_TARGET: DrawTarget> ProfilerDisplay<DRAW_TARGET> {
    /// Creates a new `ProfilerDisplay` instance with the given drawable target.
    ///
    /// # Arguments
    ///
    /// * `drawable` - The draw target to be profiled.
    pub fn new(drawable: DRAW_TARGET) -> Self {
        ProfilerDisplay {
            drawtarget: drawable,
            time_draw: Duration::from_millis(0),
            time_draw_iter: Duration::from_millis(0),
            time_fill_contiguous: Duration::from_millis(0),
            time_fill_solid: Duration::from_millis(0),
        }
    }

    /// Returns the total time spent in the `draw` operation.
    pub fn get_time_draw(&self) -> Duration {
        self.time_draw
    }

    /// Returns the total time spent in the `draw_iter` operation.
    pub fn get_time_draw_iter(&self) -> Duration {
        self.time_draw_iter
    }

    /// Returns the total time spent in the `fill_contiguous` operation.
    pub fn get_time_fill_contiguous(&self) -> Duration {
        self.time_fill_contiguous
    }

    /// Returns the total time spent in the `fill_solid` operation.
    pub fn get_time_fill_solid(&self) -> Duration {
        self.time_fill_solid
    }

    /// Returns the total time spent across all profiled draw operations.
    pub fn get_time(&self) -> Duration {
        self.time_draw + self.time_draw_iter + self.time_fill_contiguous + self.time_fill_solid
    }

    /// Resets time tracking to zero.
    pub fn reset_time(&mut self) {
        self.time_draw = Duration::from_millis(0);
        self.time_draw_iter = Duration::from_millis(0);
        self.time_fill_contiguous = Duration::from_millis(0);
        self.time_fill_solid = Duration::from_millis(0);
    }
}
