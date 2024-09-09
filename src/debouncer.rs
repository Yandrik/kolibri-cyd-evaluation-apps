/// Basic debouncer that checks 16 samples, and only if all of them are on / off, it will return
/// the value.
#[derive(Debug)]
pub struct Debouncer(u16, bool);

impl Debouncer {
    pub fn new() -> Self {
        Self(0, false)
    }

    pub fn read(&self) -> bool {
        self.1
    }

    /// Completely fill the debouncer, settng it "true".
    pub fn fill(&mut self) {
        self.0 = u16::MAX;
        self.1 = true;
    }

    /// Completely empty the debouncer, settng it "false".
    pub fn empty(&mut self) {
        self.0 = 0;
        self.1 = false;
    }

    pub fn update(&mut self, val: bool) -> bool {
        self.0 = (self.0 << 1) | (val as u16);
        if self.0 == u16::MAX {
            self.1 = true;
        } else if self.0 == 0 {
            self.1 = false;
        }

        self.1
    }
}
