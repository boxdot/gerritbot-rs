use std::str::FromStr;

use lazy_static::lazy_static;
use regex::Regex;

#[derive(Debug)]
pub enum Command {
    Enable,
    Disable,
    Status,
    Help,
    Version,
    FilterStatus,
    FilterEnable(bool),
    FilterAdd(String),
}

impl FromStr for Command {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        lazy_static! {
            static ref FILTER_REGEX: Regex = Regex::new(r"(?i)^filter (.*)$").unwrap();
        };

        Ok(match &s.trim().to_lowercase()[..] {
            "enable" => Command::Enable,
            "disable" => Command::Disable,
            "status" => Command::Status,
            "help" => Command::Help,
            "version" => Command::Version,
            "filter" => Command::FilterStatus,
            "filter enable" => Command::FilterEnable(true),
            "filter disable" => Command::FilterEnable(false),
            _ => FILTER_REGEX
                .captures(&s.trim()[..])
                .and_then(|cap| cap.get(1))
                .map(|m| Command::FilterAdd(m.as_str().to_string()))
                .ok_or(())?,
        })
    }
}
