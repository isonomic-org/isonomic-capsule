//! The engine's own log, whose last lines a panic report carries. It lives
//! in `capsule_core::log` so that every engine crate (the agents host and
//! the gateway included) writes into the same ring.

pub use capsule_core::log::{line, remember, tail, RING_LINES};

#[cfg(test)]
mod tests {
    #[test]
    fn the_ring_is_as_long_as_a_reports_tail() {
        assert_eq!(super::RING_LINES, crate::LOG_LINES);
    }
}
