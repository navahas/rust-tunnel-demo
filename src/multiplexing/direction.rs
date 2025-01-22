use std::fmt;

/// Direction of the connection relative to this node
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum ConnectionDirection {
    /// Connection listens for incoming connections
    Inbound,
    /// Connection establishes an outbound connection
    Outbound,
}

impl ConnectionDirection {
    pub fn as_str(&self) -> &'static str {
        use ConnectionDirection::{Inbound, Outbound};
        match self {
            Inbound => "inbound",
            Outbound => "outbound",
        }
    }
}

impl fmt::Display for ConnectionDirection {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}
