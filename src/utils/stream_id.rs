use std::fmt;

/// Implement this trait on any `Stream` that can identify itself by [Id](self::Id).
pub trait StreamId {
    fn stream_id(&self) -> Id;
}

/// An integer stream ID
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Id(u32);

impl Id {
    /// New Id
    pub fn new(val: u32) -> Self {
        Self(val)
    }

    /// Returns the stream ID as a u32
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.as_u32())
    }
}
