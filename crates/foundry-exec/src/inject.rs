#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    W1,
    W2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultInjector {
    point: Option<CrashPoint>,
    armed: bool,
}

impl FaultInjector {
    pub const fn new() -> Self {
        Self {
            point: None,
            armed: false,
        }
    }

    pub const fn armed_at(point: CrashPoint) -> Self {
        Self {
            point: Some(point),
            armed: true,
        }
    }

    pub fn arm(&mut self, point: CrashPoint) {
        self.point = Some(point);
        self.armed = true;
    }

    pub fn fire(&mut self, point: CrashPoint) -> Result<(), CrashPoint> {
        if self.armed && self.point == Some(point) {
            self.armed = false;
            self.point = None;
            Err(point)
        } else {
            Ok(())
        }
    }
}

impl Default for FaultInjector {
    fn default() -> Self {
        Self::new()
    }
}
