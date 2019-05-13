use std::collections::HashSet;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

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
        serde_json::from_slice(s.as_bytes())
    }
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
];

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        if let UserFlags::Default = self {
            true
        } else {
            false
        }
    }

    pub fn reset(&mut self) {
        std::mem::replace(self, UserFlags::Default);
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
                std::mem::replace(self, UserFlags::Custom(set));
            }
            UserFlags::Custom(ref mut set) => set_flag(set),
        }
    }
}
