//! Snowflake-compatible 63-bit identifiers.
//!
//! Layout, from most to least significant: 41 bits of milliseconds since
//! [`EPOCH_MS`], 10 bits of node id, and 12 bits of per-millisecond sequence.

use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Custom epoch: 2025-01-01T00:00:00Z. This value must never change.
pub const EPOCH_MS: u64 = 1_735_689_600_000;

const TIMESTAMP_BITS: u64 = 41;
const NODE_BITS: u64 = 10;
const SEQUENCE_BITS: u64 = 12;
const MAX_NODE_ID: u16 = (1 << NODE_BITS) - 1;
const MAX_SEQUENCE: u16 = (1 << SEQUENCE_BITS) - 1;
const MAX_TIMESTAMP: u64 = (1 << TIMESTAMP_BITS) - 1;

#[derive(Debug, thiserror::Error)]
pub enum IdError {
    #[error("node id must be between 0 and {MAX_NODE_ID}, got {0}")]
    InvalidNodeId(u16),
    #[error("system clock is before the Snowflake epoch")]
    ClockBeforeEpoch,
    #[error("Snowflake timestamp overflow")]
    TimestampOverflow,
    #[error("cannot persist ID generator state: {0}")]
    State(#[from] std::io::Error),
    #[error("invalid ID generator state")]
    InvalidState,
}

type Clock = Box<dyn FnMut() -> u64 + Send>;

pub struct Snowflake {
    node_id: u16,
    last_timestamp: Option<u64>,
    sequence: u16,
    now_ms: Clock,
    state_path: Option<PathBuf>,
}

impl Snowflake {
    pub fn new(node_id: u16) -> Result<Self, IdError> {
        Self::with_clock(node_id, Box::new(system_now_ms))
    }

    /// The caller must hold the application lock for the generator's lifetime.
    pub fn persistent(node_id: u16, path: &Path) -> Result<Self, IdError> {
        let mut generator = Self::new(node_id)?;
        match fs::read_to_string(path) {
            Ok(text) => {
                let id: u64 = text.trim().parse().map_err(|_| IdError::InvalidState)?;
                if id > i64::MAX as u64 {
                    return Err(IdError::InvalidState);
                }
                if ((id >> SEQUENCE_BITS) & u64::from(MAX_NODE_ID)) == u64::from(node_id) {
                    generator.last_timestamp = Some(id >> (NODE_BITS + SEQUENCE_BITS));
                    generator.sequence = (id & u64::from(MAX_SEQUENCE)) as u16;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        generator.state_path = Some(path.to_owned());
        Ok(generator)
    }

    fn with_clock(node_id: u16, now_ms: Clock) -> Result<Self, IdError> {
        if node_id > MAX_NODE_ID {
            return Err(IdError::InvalidNodeId(node_id));
        }
        Ok(Self {
            node_id,
            last_timestamp: None,
            sequence: 0,
            now_ms,
            state_path: None,
        })
    }

    pub fn node_id(&self) -> u16 {
        self.node_id
    }

    pub fn next_id(&mut self) -> Result<i64, IdError> {
        let mut timestamp = self.current_timestamp()?;

        if let Some(last) = self.last_timestamp {
            if timestamp < last {
                // The clock moved backwards. Keep issuing IDs from the last
                // timestamp so this process remains monotonic.
                timestamp = last;
            }

            if timestamp == last {
                if self.sequence == MAX_SEQUENCE {
                    timestamp = self.next_timestamp_after(last)?;
                    self.sequence = 0;
                } else {
                    self.sequence += 1;
                }
            } else {
                self.sequence = 0;
            }
        } else {
            self.sequence = 0;
        }

        self.last_timestamp = Some(timestamp);

        let id = (timestamp << (NODE_BITS + SEQUENCE_BITS))
            | (u64::from(self.node_id) << SEQUENCE_BITS)
            | u64::from(self.sequence);
        if let Some(path) = &self.state_path {
            let parent = path.parent().ok_or(IdError::InvalidState)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            write!(file, "{id}")?;
            file.as_file().sync_all()?;
            file.persist(path).map_err(|error| error.error)?;
            #[cfg(unix)]
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(id as i64)
    }

    fn current_timestamp(&mut self) -> Result<u64, IdError> {
        let now = (self.now_ms)();
        let timestamp = now.checked_sub(EPOCH_MS).ok_or(IdError::ClockBeforeEpoch)?;
        if timestamp > MAX_TIMESTAMP {
            return Err(IdError::TimestampOverflow);
        }
        Ok(timestamp)
    }

    /// Waits for the clock to pass `last`. If the clock is stuck (for example a
    /// backwards adjustment), fall back to the next millisecond so the
    /// generator cannot hang forever.
    fn next_timestamp_after(&mut self, last: u64) -> Result<u64, IdError> {
        for _ in 0..5 {
            let now = self.current_timestamp()?;
            if now > last {
                return Ok(now);
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        last.checked_add(1)
            .filter(|timestamp| *timestamp <= MAX_TIMESTAMP)
            .ok_or(IdError::TimestampOverflow)
    }
}

fn system_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    fn at(milliseconds: u64) -> u64 {
        EPOCH_MS + milliseconds
    }

    fn generator(node_id: u16, times: Vec<u64>) -> Snowflake {
        let mut queue: VecDeque<u64> = times.into();
        Snowflake::with_clock(
            node_id,
            Box::new(move || queue.pop_front().expect("fake clock ran out of values")),
        )
        .unwrap()
    }

    #[test]
    fn persistent_generator_resumes_sequence_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state");
        let mut first = generator(1, vec![at(100)]);
        first.state_path = Some(path.clone());
        let first_id = first.next_id().unwrap();
        let mut second = Snowflake::persistent(1, &path).unwrap();
        second.now_ms = Box::new(|| at(100));
        assert!(second.next_id().unwrap() > first_id);
    }

    #[test]
    fn encodes_timestamp_node_and_sequence() {
        let mut ids = generator(3, vec![at(10), at(10), at(11)]);

        let first = ids.next_id().unwrap() as u64;
        let second = ids.next_id().unwrap() as u64;
        let third = ids.next_id().unwrap() as u64;

        assert!(first < second && second < third);
        assert_eq!(first >> (NODE_BITS + SEQUENCE_BITS), 10);
        assert_eq!((first >> SEQUENCE_BITS) & u64::from(MAX_NODE_ID), 3);
        assert_eq!(first & u64::from(MAX_SEQUENCE), 0);
        assert_eq!(second & u64::from(MAX_SEQUENCE), 1);
        assert_eq!(third & u64::from(MAX_SEQUENCE), 0);
    }

    #[test]
    fn stays_monotonic_when_the_clock_moves_backwards() {
        let mut ids = generator(1, vec![at(10), at(4), at(11)]);

        let first = ids.next_id().unwrap() as u64;
        let second = ids.next_id().unwrap() as u64;
        let third = ids.next_id().unwrap() as u64;

        assert!(first < second && second < third);
        assert_eq!(second >> (NODE_BITS + SEQUENCE_BITS), 10);
        assert_eq!(third >> (NODE_BITS + SEQUENCE_BITS), 11);
    }

    #[test]
    fn advances_to_the_next_millisecond_when_the_sequence_is_exhausted() {
        let mut times = vec![at(0); usize::from(MAX_SEQUENCE) + 2];
        times.push(at(1));
        let mut ids = generator(0, times);

        let mut previous = ids.next_id().unwrap() as u64;
        for _ in 0..=usize::from(MAX_SEQUENCE) {
            let next = ids.next_id().unwrap() as u64;
            assert!(next > previous);
            previous = next;
        }
        assert_eq!(previous >> (NODE_BITS + SEQUENCE_BITS), 1);
        assert_eq!(previous & u64::from(MAX_SEQUENCE), 0);
    }

    #[test]
    fn rejects_node_ids_outside_the_ten_bit_range() {
        assert!(matches!(
            Snowflake::new(MAX_NODE_ID + 1),
            Err(IdError::InvalidNodeId(_))
        ));
    }

    #[test]
    fn rejects_timestamps_before_the_epoch() {
        let mut ids = generator(0, vec![EPOCH_MS - 1]);
        assert!(matches!(ids.next_id(), Err(IdError::ClockBeforeEpoch)));
    }
}
