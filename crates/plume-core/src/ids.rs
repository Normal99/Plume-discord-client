use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Milliseconds from the Unix epoch to the start of 2015, Discord's epoch.
pub const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

/// A Discord ID. Discord sends these as strings (they don't fit in a JS
/// number), but they're 64-bit integers, so we keep them as `u64`: 8 bytes,
/// `Copy`, and cheap to hash and compare.
///
/// The top 42 bits are a timestamp, so newer IDs sort after older ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Snowflake(pub u64);

impl Snowflake {
    /// The smallest ID created at `unix_ms`. Used for nonces and for
    /// "messages before/after this time" queries.
    pub fn from_timestamp_ms(unix_ms: u64) -> Self {
        Snowflake(unix_ms.saturating_sub(DISCORD_EPOCH_MS) << 22)
    }

    /// When this ID was created, in Unix milliseconds.
    pub fn timestamp_ms(self) -> u64 {
        (self.0 >> 22) + DISCORD_EPOCH_MS
    }
}

impl fmt::Display for Snowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Snowflake {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Snowflake)
    }
}

impl Serialize for Snowflake {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Always as a string, like Discord does.
        serializer.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Snowflake {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(SnowflakeVisitor)
    }
}

/// Accepts `"123"` (what Discord normally sends) and `123` (a few places
/// send plain numbers).
struct SnowflakeVisitor;

impl Visitor<'_> for SnowflakeVisitor {
    type Value = Snowflake;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Discord ID as a string or number")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Snowflake, E> {
        value
            .parse()
            .map_err(|_| E::invalid_value(de::Unexpected::Str(value), &self))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Snowflake, E> {
        Ok(Snowflake(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Snowflake, E> {
        u64::try_from(value)
            .map(Snowflake)
            .map_err(|_| E::invalid_value(de::Unexpected::Signed(value), &self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_strings_and_numbers() {
        let ids: Vec<Snowflake> = serde_json::from_str(r#"["175928847299117063", 42]"#).unwrap();
        assert_eq!(ids, [Snowflake(175928847299117063), Snowflake(42)]);
        assert!(serde_json::from_str::<Snowflake>(r#""nope""#).is_err());
        assert!(serde_json::from_str::<Snowflake>("-1").is_err());
    }

    #[test]
    fn serializes_as_string() {
        assert_eq!(serde_json::to_string(&Snowflake(7)).unwrap(), r#""7""#);
    }

    #[test]
    fn timestamps_round_trip() {
        // The example from Discord's docs.
        assert_eq!(Snowflake(175928847299117063).timestamp_ms(), 1_462_015_105_796);
        let id = Snowflake::from_timestamp_ms(1_462_015_105_796);
        assert_eq!(id.timestamp_ms(), 1_462_015_105_796);
        assert!(id <= Snowflake(175928847299117063));
    }
}
