use std::borrow::Borrow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};

use gerritbot_spark as spark;

use super::BotError;

#[derive(Debug, Clone)]
struct Filter {
    pub regex: Regex,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize)]
struct FilterForSerialize<'a> {
    regex: &'a str,
    enabled: bool,
}

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
enum UserFlags {
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
    fn contains(&self, flag: UserFlag) -> bool {
        match self {
            UserFlags::Default => DEFAULT_FLAGS
                .iter()
                .any(|default_flag| default_flag == &flag),
            UserFlags::Custom(set) => set.contains(&flag),
        }
    }

    fn is_default(&self) -> bool {
        if let UserFlags::Default = self {
            true
        } else {
            false
        }
    }

    fn reset(&mut self) {
        std::mem::replace(self, UserFlags::Default);
    }

    fn set(&mut self, flag: UserFlag, value: bool) {
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

/// Serialize the filter by storing the regex as a string.
fn serialize_filter<S>(filter: &Option<Filter>, serializer: S) -> Result<S::Ok, S::Error>
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
fn deserialize_filter<'de, D>(deserializer: D) -> Result<Option<Filter>, D::Error>
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    // Legacy attribute.  Keep so we don't drop it on deserialize, serialize.
    // Should be removed later.
    #[serde(skip_serializing_if = "Option::is_none")]
    spark_person_id: Option<String>,
    /// email of the user; assumed to be the same in Spark and Gerrit
    email: spark::Email,
    #[serde(skip_serializing_if = "UserFlags::is_default", default)]
    flags: UserFlags,
    enabled: bool,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_filter",
        deserialize_with = "deserialize_filter",
        default
    )]
    filter: Option<Filter>,
}

impl User {
    fn new(email: spark::Email) -> Self {
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    users: Vec<User>,
    #[serde(skip_serializing, skip_deserializing)]
    email_index: HashMap<spark::Email, usize>,
}

impl State {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn load<P>(filename: P) -> Result<Self, BotError>
    where
        P: AsRef<Path>,
    {
        let f = File::open(filename)?;

        serde_json::from_reader(f)
            .map(|mut state: Self| {
                state.index_users();
                state
            })
            .map_err(BotError::from)
    }

    fn index_users(&mut self) {
        for (user_pos, user) in self.users.iter().enumerate() {
            self.email_index.insert(user.email.clone(), user_pos);
        }
    }

    pub fn num_users(&self) -> usize {
        self.users.len()
    }

    // Note: This method is not idempotent, and in particular, when adding the same user twice,
    // it will completely mess up the indexes.
    pub fn add_user<'a>(&'a mut self, email: &spark::EmailRef) -> &'a mut User {
        let user_pos = self.users.len();
        self.users.push(User::new(email.to_owned()));
        self.email_index.insert(email.to_owned(), user_pos);
        self.users.last_mut().unwrap()
    }

    fn find_or_add_user_by_email<'a>(&'a mut self, email: &spark::EmailRef) -> &'a mut User {
        let pos = self.users.iter().position(|u| u.email == email);
        let user: &'a mut User = match pos {
            Some(pos) => &mut self.users[pos],
            None => self.add_user(email),
        };
        user
    }

    fn find_user_mut<'a, P: ?Sized>(&'a mut self, email: &P) -> Option<&'a mut User>
    where
        spark::Email: std::borrow::Borrow<P>,
        P: std::hash::Hash + Eq,
    {
        self.email_index
            .get(email)
            .cloned()
            .map(move |pos| &mut self.users[pos])
    }

    pub fn find_user<'a, P: ?Sized>(&'a self, email: &P) -> Option<&'a User>
    where
        spark::Email: std::borrow::Borrow<P>,
        P: std::hash::Hash + Eq,
    {
        self.email_index
            .get(email)
            .cloned()
            .map(|pos| &self.users[pos])
    }

    pub fn find_user_by_email<'a, E: ?Sized>(&self, email: &E) -> Option<&User>
    where
        spark::Email: std::borrow::Borrow<E>,
        E: std::hash::Hash + Eq,
    {
        self.email_index.get(email).map(|pos| &self.users[*pos])
    }

    pub fn reset_flags(&mut self, email: &spark::EmailRef) -> &User {
        let user = self.find_or_add_user_by_email(email);
        user.flags.reset();
        user
    }

    pub fn set_flag(&mut self, email: &spark::EmailRef, flag: UserFlag, value: bool) -> &User {
        let user = self.find_or_add_user_by_email(email);
        user.flags.set(flag, value);
        user
    }

    pub fn enable<'a>(&'a mut self, email: &spark::EmailRef, enabled: bool) -> &'a User {
        let user: &'a mut User = self.find_or_add_user_by_email(email);
        user.enabled = enabled;
        user
    }

    pub fn add_filter(
        &mut self,
        email: &spark::EmailRef,
        filter: &str,
    ) -> Result<(), regex::Error> {
        let user = self.find_or_add_user_by_email(email);
        user.filter = Some(Filter {
            regex: Regex::new(filter)?,
            enabled: true,
        });
        Ok(())
    }

    /// Get the filter for the given user given the user exists and has a filter
    /// configured.
    pub fn get_filter(&self, email: &spark::EmailRef) -> Option<(&str, bool)> {
        self.find_user(email)
            .and_then(|u| u.filter.as_ref())
            .map(|f| (f.regex.as_str(), f.enabled))
    }

    /// Enable or disable the configured filter for the user and return it given
    /// that the user exists and has a filter configured. Error means the user
    /// doesn't exist or doesn't have a filter configured.
    pub fn enable_and_get_filter(
        &mut self,
        email: &spark::EmailRef,
        enabled: bool,
    ) -> Result<&str, ()> {
        self.find_user_mut(email)
            .and_then(|u| u.filter.as_mut())
            .map(|f| {
                f.enabled = enabled;
                f.regex.as_str()
            })
            .ok_or(())
    }

    pub fn users(&self) -> impl Iterator<Item = &User> + Clone {
        self.users.iter()
    }

    pub fn is_filtered(&self, user: &User, msg: &str) -> bool {
        user.filter
            .as_ref()
            .map(|f| f.enabled && f.regex.is_match(msg))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod test {
    use spark::EmailRef;

    use super::*;

    #[test]
    fn test_add_user() {
        let mut state = State::new();
        state.add_user(EmailRef::new("some@example.com"));
        assert_eq!(state.users.len(), 1);
        assert_eq!(state.email_index.len(), 1);
        assert_eq!(state.users[0].email, EmailRef::new("some@example.com"));
        assert_eq!(
            state.email_index.get(EmailRef::new("some@example.com")),
            Some(&0)
        );
        assert_eq!(
            state.email_index.get(EmailRef::new("some@example.com")),
            Some(&0)
        );

        state.add_user(EmailRef::new("some_2@example.com"));
        assert_eq!(state.users.len(), 2);
        assert_eq!(state.email_index.len(), 2);
        assert_eq!(state.users[1].email, EmailRef::new("some_2@example.com"));
        assert_eq!(
            state.email_index.get(EmailRef::new("some_2@example.com")),
            Some(&1)
        );
        assert_eq!(
            state.email_index.get(EmailRef::new("some_2@example.com")),
            Some(&1)
        );

        let user = state.find_user(EmailRef::new("some@example.com"));
        assert!(user.is_some());
        assert_eq!(user.unwrap().email, EmailRef::new("some@example.com"));

        let user = state.find_user(EmailRef::new("some_2@example.com"));
        assert!(user.is_some());
        assert_eq!(user.unwrap().email, EmailRef::new("some_2@example.com"));
    }

    #[test]
    fn add_invalid_filter_for_existing_user() {
        let mut state = State::new();
        state.add_user(EmailRef::new("some@example.com"));
        let res = state.add_filter(EmailRef::new("some@example.com"), ".some_weard_regex/[");
        assert!(res.is_err());
        assert!(state
            .users
            .iter()
            .position(|u| u.email == EmailRef::new("some@example.com") && u.filter.is_none())
            .is_some());

        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), true);
        assert_eq!(res, Err(()));
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), false);
        assert_eq!(res, Err(()));
    }

    #[test]
    fn add_valid_filter_for_existing_user() {
        let mut state = State::new();
        state.add_user(EmailRef::new("some@example.com"));

        let res = state.add_filter(EmailRef::new("some@example.com"), ".*some_word.*");
        assert!(res.is_ok());
        assert!(state
            .users
            .iter()
            .position(|u| u.email == EmailRef::new("some@example.com")
                && u.filter.as_ref().map(|f| f.regex.as_str()) == Some(".*some_word.*"))
            .is_some());

        {
            let filter = state.get_filter(EmailRef::new("some@example.com"));
            assert_eq!(filter, Some((".*some_word.*", true)));
        }
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), false);
        assert_eq!(res, Ok(".*some_word.*"));
        assert!(state
            .users
            .iter()
            .position(|u| u.email == EmailRef::new("some@example.com")
                && u.filter.as_ref().map(|f| f.enabled) == Some(false))
            .is_some());
        {
            let filter = state.get_filter(EmailRef::new("some@example.com"));
            assert_eq!(filter, Some((".*some_word.*", false)));
        }
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), true);
        assert_eq!(res, Ok(".*some_word.*"));
        assert!(state
            .users
            .iter()
            .position(|u| u.email == EmailRef::new("some@example.com")
                && u.filter.as_ref().map(|f| f.enabled) == Some(true))
            .is_some());
        {
            let filter = state.get_filter(EmailRef::new("some@example.com"));
            assert_eq!(filter, Some((".*some_word.*", true)));
        }
    }

    #[test]
    fn add_valid_filter_for_non_existing_user() {
        let mut state = State::new();
        let res = state.add_filter(EmailRef::new("some@example.com"), ".*some_word.*");
        assert_eq!(res, Ok(()));
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), true);
        assert_eq!(res, Ok(".*some_word.*"));
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), false);
        assert_eq!(res, Ok(".*some_word.*"));
    }

    #[test]
    fn add_valid_filter_for_disabled_user() {
        let mut state = State::new();
        state.add_user(EmailRef::new("some@example.com"));
        state.users[0].enabled = false;

        let res = state.add_filter(EmailRef::new("some@example.com"), ".*some_word.*");
        assert_eq!(res, Ok(()));
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), true);
        assert_eq!(res, Ok(".*some_word.*"));
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), false);
        assert_eq!(res, Ok(".*some_word.*"));
    }

    #[test]
    fn enable_non_configured_filter_for_existing_user() {
        let mut state = State::new();
        state.add_user(EmailRef::new("some@example.com"));

        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), true);
        assert_eq!(res, Err(()));
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), false);
        assert_eq!(res, Err(()));
    }
}
