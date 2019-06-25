use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Filter {
    pub regex: Regex,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize)]
struct FilterForSerialize<'a> {
    regex: &'a str,
    enabled: bool,
}

/// Serialize the filter by storing the regex as a string.
pub(super) fn serialize_filter<S>(filter: &Option<Filter>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    filter
        .as_ref()
        .map(|f| FilterForSerialize {
            regex: f.regex.as_str(),
            enabled: f.enabled,
        })
        .serialize(serializer)
}

/// Deserialize the filter by compiling the regex.
pub(super) fn deserialize_filter<'de, D>(deserializer: D) -> Result<Option<Filter>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let maybe_filter = Option::<FilterForSerialize>::deserialize(deserializer)?;

    maybe_filter
        .map(|f| {
            Regex::new(f.regex)
                .map(|regex| Filter {
                    regex,
                    enabled: f.enabled,
                })
                .map_err(|e| {
                    <D::Error as serde::de::Error>::custom(format!("invalid regex: {}", e))
                })
        })
        .transpose()
}
