use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A Discord snowflake.
///
/// Accepts both JSON/TOML strings and integers so configuration files can use
/// the form that is least surprising for the author.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Snowflake(pub u64);

impl Snowflake {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Parse a decimal snowflake.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is not a positive decimal integer.
    pub fn parse(value: &str) -> Result<Self, String> {
        let parsed: u64 = value
            .trim()
            .parse()
            .map_err(|_| format!("invalid snowflake: {value:?}"))?;
        if parsed == 0 {
            return Err("snowflake must be greater than zero".to_owned());
        }
        Ok(Self(parsed))
    }
}

impl fmt::Display for Snowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for Snowflake {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Snowflake {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = Snowflake;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Discord snowflake as a string or integer")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value == 0 {
                    Err(E::custom("snowflake must be greater than zero"))
                } else {
                    Ok(Snowflake(value))
                }
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value <= 0 {
                    Err(E::custom("snowflake must be greater than zero"))
                } else {
                    Ok(Snowflake(value as u64))
                }
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Snowflake::parse(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::Snowflake;

    #[derive(serde::Deserialize)]
    struct Wrapper {
        id: Snowflake,
    }

    #[test]
    fn parses_string_and_integer() {
        assert_eq!(Snowflake::parse("123").unwrap(), Snowflake(123));
        let from_str: Wrapper = toml::from_str("id = \"456\"").unwrap();
        let from_int: Wrapper = toml::from_str("id = 789").unwrap();
        assert_eq!(from_str.id, Snowflake(456));
        assert_eq!(from_int.id, Snowflake(789));
    }

    #[test]
    fn rejects_zero_and_garbage() {
        assert!(Snowflake::parse("0").is_err());
        assert!(Snowflake::parse("abc").is_err());
    }
}
