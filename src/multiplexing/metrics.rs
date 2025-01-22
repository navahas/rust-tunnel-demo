use once_cell::sync::Lazy;
use tari_metrics::IntCounter;

pub static TOTAL_BYTES_READ: Lazy<IntCounter> = Lazy::new(|| {
    tari_metrics::register_int_counter(
        "comms::substream::total_bytes_read",
        "The total inbound bytes",
    )
    .unwrap()
});

pub static TOTAL_BYTES_WRITTEN: Lazy<IntCounter> = Lazy::new(|| {
    tari_metrics::register_int_counter(
        "comms::substream::total_bytes_written",
        "The total outbound bytes",
    )
    .unwrap()
});
