//! Gate classes and modes (H-1, H-1.1). Contract: part of I-1 and I-3.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// What a tool call does to the world (H-1). Every call is classified into exactly one.
///
/// The derive order is the severity order used by `max()` for program classes (B-7) and
/// floors (H-1.5), and it is part of the contract (D-36):
/// `Read < Fill < Navigate < Submit < Send < Account < Delete < Pay < Challenge`.
/// Classes whose ask times out to *skip* (Delete, Pay) outrank those that time out to
/// *proceed* (Send, Account), so taking the max never turns a skip into a proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Class {
    Read,
    Fill,
    Navigate,
    Submit,
    Send,
    Account,
    Delete,
    Pay,
    Challenge,
}

impl Class {
    pub const ALL: [Class; 9] = [
        Class::Read,
        Class::Fill,
        Class::Navigate,
        Class::Submit,
        Class::Send,
        Class::Account,
        Class::Delete,
        Class::Pay,
        Class::Challenge,
    ];

    /// The spelling used in `gate.rules.class`, `ledger.events.class` and tool `@gate` headers.
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Read => "Read",
            Class::Fill => "Fill",
            Class::Navigate => "Navigate",
            Class::Submit => "Submit",
            Class::Send => "Send",
            Class::Account => "Account",
            Class::Delete => "Delete",
            Class::Pay => "Pay",
            Class::Challenge => "Challenge",
        }
    }

    /// `max(self, floor)`: a floor can raise a class, never lower it (H-1.5, D-29).
    pub fn at_least(self, floor: Class) -> Class {
        self.max(floor)
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown gate class {0:?}")]
pub struct UnknownClass(pub String);

impl FromStr for Class {
    type Err = UnknownClass;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Class::ALL
            .into_iter()
            .find(|c| c.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| UnknownClass(s.to_string()))
    }
}

/// What the gate does with a class (H-1.1). Stored lowercase in `gate.rules.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Allow,
    Ask,
    Stop,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Allow => "allow",
            Mode::Ask => "ask",
            Mode::Stop => "stop",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown gate mode {0:?}")]
pub struct UnknownMode(pub String);

impl FromStr for Mode {
    type Err = UnknownMode;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "allow" => Ok(Mode::Allow),
            "ask" => Ok(Mode::Ask),
            "stop" => Ok(Mode::Stop),
            _ => Err(UnknownMode(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_order_is_the_contract() {
        let mut sorted = Class::ALL;
        sorted.sort();
        assert_eq!(sorted, Class::ALL);
        assert!(Class::Pay > Class::Account && Class::Delete > Class::Send);
        assert_eq!(Class::Submit.at_least(Class::Pay), Class::Pay);
        assert_eq!(Class::Pay.at_least(Class::Read), Class::Pay);
    }

    #[test]
    fn class_spelling_round_trips() {
        for c in Class::ALL {
            assert_eq!(c.as_str().parse::<Class>().unwrap(), c);
            assert_eq!(serde_json::to_string(&c).unwrap(), format!("\"{c}\""));
        }
        assert!("Shout".parse::<Class>().is_err());
    }

    #[test]
    fn mode_spelling_round_trips() {
        for m in [Mode::Allow, Mode::Ask, Mode::Stop] {
            assert_eq!(m.as_str().parse::<Mode>().unwrap(), m);
            assert_eq!(serde_json::to_string(&m).unwrap(), format!("\"{m}\""));
        }
    }
}
