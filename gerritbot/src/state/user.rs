use std::borrow::Borrow;

use serde::{Deserialize, Serialize};

use gerritbot_spark as spark;

use super::filter::{deserialize_filter, serialize_filter, Filter};
use super::flags::{UserFlag, UserFlags};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    // Legacy attribute.  Keep so we don't drop it on deserialize, serialize.
    // Should be removed later.
    #[serde(skip_serializing_if = "Option::is_none")]
    spark_person_id: Option<String>,
    /// email of the user; assumed to be the same in Spark and Gerrit
    pub(super) email: spark::Email,
    #[serde(skip_serializing_if = "UserFlags::is_default", default)]
    pub(super) flags: UserFlags,
    pub(super) enabled: bool,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_filter",
        deserialize_with = "deserialize_filter",
        default
    )]
    pub(super) filter: Option<Filter>,
}

impl User {
    pub(super) fn new(email: spark::Email) -> Self {
        Self {
            spark_person_id: None,
            email: email,
            filter: None,
            enabled: true,
            flags: UserFlags::Default,
        }
    }

    pub fn email(&self) -> &spark::EmailRef {
        &self.email
    }

    pub fn has_any_flag<I, F>(&self, flags: I) -> bool
    where
        I: IntoIterator<Item = F>,
        F: Borrow<UserFlag>,
    {
        self.enabled
            && flags
                .into_iter()
                .any(|flag| self.flags.contains(*flag.borrow()))
    }

    pub fn has_flag(&self, flag: UserFlag) -> bool {
        self.has_any_flag(&[flag])
    }
}
