use std::collections::HashSet;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[allow(clippy::enum_variant_names)]
#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UserFlag {
    /// User wants notification messages for reviews with approvals.
    NotifyReviewApprovals,
    /// User wants notification messages for review comments without approvals.
    NotifyReviewComments,
    /// User wants notification messages for reviews with inline comments.
    NotifyReviewInlineComments,
    /// User wants notification messages when added as reviewer to a change.
    NotifyReviewerAdded,
    /// User wants notification messages for review comments without approvals.
    NotifyReviewResponses,
    /// User wants notification messages for merged changes.
    NotifyChangeMerged,
    /// User wants notification messages for abandoned changes.
    NotifyChangeAbandoned,
}

impl Display for UserFlag {
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        if let Ok(serde_json::Value::String(s)) = serde_json::to_value(self) {
            write!(f, "{}", s)
        } else {
            panic!("failed to encode flag")
        }
    }
}

impl FromStr for UserFlag {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_slice(format!("\"{}\"", s).as_bytes())
    }
}

#[cfg(test)]
mod test_flag {
    use super::UserFlag;

    macro_rules! test_from_to_string {
        ($name:ident, $s:expr, $f:expr $( , )?) => {
            #[test]
            fn $name() {
                assert_eq!($s.parse::<UserFlag>().expect("parse failed"), $f);
                assert_eq!($f.to_string(), $s);
            }
        };
    }
    macro_rules! test_parse_fail {
        ($name:ident, $s:expr) => {
            #[test]
            fn $name() {
                $s.parse::<UserFlag>().expect_err("did not fail");
            }
        };
    }

    test_from_to_string!(
        notify_review_approvals,
        "notify_review_approvals",
        UserFlag::NotifyReviewApprovals,
    );
    test_from_to_string!(
        notify_review_comments,
        "notify_review_comments",
        UserFlag::NotifyReviewComments,
    );
    test_from_to_string!(
        notify_review_inline_comments,
        "notify_review_inline_comments",
        UserFlag::NotifyReviewInlineComments,
    );
    test_from_to_string!(
        notify_reviewer_added,
        "notify_reviewer_added",
        UserFlag::NotifyReviewerAdded,
    );

    test_parse_fail!(unknown_flag, "unknown_flag");
    test_parse_fail!(integer, "123");
    test_parse_fail!(quotation_mark, "\"");
}

/// Default flags for users that haven't enabled or disabled anything specific.
const DEFAULT_FLAGS: &[UserFlag] = &[
    UserFlag::NotifyReviewApprovals,
    UserFlag::NotifyReviewInlineComments,
    UserFlag::NotifyReviewerAdded,
];

/// All flags that deal with review comments.
pub const REVIEW_COMMENT_FLAGS: &[UserFlag] = &[
    UserFlag::NotifyReviewApprovals,
    UserFlag::NotifyReviewComments,
    UserFlag::NotifyReviewInlineComments,
];

/// All flags that deal with notifications.
pub const NOTIFICATION_FLAGS: &[UserFlag] = &[
    UserFlag::NotifyReviewApprovals,
    UserFlag::NotifyReviewComments,
    UserFlag::NotifyReviewInlineComments,
    UserFlag::NotifyReviewerAdded,
    UserFlag::NotifyReviewResponses,
    UserFlag::NotifyChangeMerged,
    UserFlag::NotifyChangeAbandoned,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(super) enum UserFlags {
    Default,
    // Note: this could be optimized into bitflags to make it faster and avoid
    // allocation.
    Custom(HashSet<UserFlag>),
}

impl Default for UserFlags {
    fn default() -> Self {
        UserFlags::Default
    }
}

impl UserFlags {
    pub fn contains(&self, flag: UserFlag) -> bool {
        match self {
            UserFlags::Default => DEFAULT_FLAGS
                .iter()
                .any(|default_flag| default_flag == &flag),
            UserFlags::Custom(set) => set.contains(&flag),
        }
    }

    pub fn is_default(&self) -> bool {
        matches!(self, UserFlags::Default)
    }

    pub fn reset(&mut self) {
        *self = UserFlags::Default;
    }

    pub fn set(&mut self, flag: UserFlag, value: bool) {
        let set_flag = |set: &mut HashSet<UserFlag>| {
            if value {
                set.insert(flag);
            } else {
                set.remove(&flag);
            }
        };

        match self {
            UserFlags::Default => {
                let mut set = DEFAULT_FLAGS.iter().cloned().collect();
                set_flag(&mut set);
                *self = UserFlags::Custom(set);
            }
            UserFlags::Custom(ref mut set) => set_flag(set),
        }
    }
}
