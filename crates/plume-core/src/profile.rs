//! How much Plume keeps in memory. See docs/11-usage-profiles.md.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Feather,
    Standard,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub tier: Tier,
    /// How many channels keep their messages in memory. The least recently
    /// opened ones are dropped first (they reload when you open them again).
    pub channels_in_memory: usize,
    /// The newest messages kept per channel.
    pub messages_per_channel: usize,
}

impl Profile {
    pub const FEATHER: Profile = Profile {
        tier: Tier::Feather,
        channels_in_memory: 2,
        messages_per_channel: 50,
    };

    pub const STANDARD: Profile = Profile {
        tier: Tier::Standard,
        channels_in_memory: 8,
        messages_per_channel: 200,
    };

    pub const FULL: Profile = Profile {
        tier: Tier::Full,
        channels_in_memory: 16,
        messages_per_channel: 500,
    };

    pub fn from_name(name: &str) -> Option<Profile> {
        match name.to_ascii_lowercase().as_str() {
            "feather" => Some(Profile::FEATHER),
            "standard" => Some(Profile::STANDARD),
            "full" => Some(Profile::FULL),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_case_insensitive() {
        assert_eq!(Profile::from_name("Standard"), Some(Profile::STANDARD));
        assert_eq!(Profile::from_name("potato"), None);
    }
}
