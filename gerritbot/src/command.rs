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

#[cfg(test)]
mod test {
    use assert_matches::assert_matches;

    use super::Command;

    macro_rules! test_parse {
        ($name:ident, $s:expr, $( $c:tt )+) => {
            #[test]
            fn $name() {
                assert_matches!($s.parse::<Command>().expect("parse failed"), $( $c )+);
            }
        };

        ($name:ident, $c:pat) => {
            test_parse!($name, stringify!($name), $c);
        };
    }

    macro_rules! test_parse_fail {
        ($name:ident, $s:expr) => {
            #[test]
            fn $name() {
                assert_matches!($s.parse::<Command>().expect_err("parse didn't fail"), ());
            }
        };
    }

    test_parse!(enable, Command::Enable);
    test_parse!(enable_with_whitespace, "\t\t   enable\n\n", Command::Enable);
    test_parse!(enable_mixed_case, "EnAbLe", Command::Enable);
    test_parse!(disable, Command::Disable);
    test_parse!(status, Command::Status);
    test_parse!(help, Command::Help);
    test_parse!(version, Command::Version);
    test_parse!(filter, Command::FilterStatus);
    test_parse!(filter_enable, "filter enable", Command::FilterEnable(true));
    test_parse!(
        filter_disable,
        "filter disable",
        Command::FilterEnable(false)
    );
    test_parse!(
        filter_set,
        "filter abc def",
        Command::FilterAdd(ref s) if s == "abc def"
    );
    test_parse!(
        filter_set_with_whitespace,
        "filter  abc def ",
        Command::FilterAdd(ref s) if s == " abc def"
    );

    test_parse_fail!(unknown_command, "unknown");
}
