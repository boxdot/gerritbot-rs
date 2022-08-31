use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};

use gerritbot_spark as spark;

use super::BotError;

mod filter;
mod flags;
mod user;

use filter::Filter;
pub use flags::{UserFlag, NOTIFICATION_FLAGS, REVIEW_COMMENT_FLAGS};
pub use user::User;

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
            self.email_index.insert(user.email().to_owned(), user_pos);
        }
    }

    pub fn num_users(&self) -> usize {
        self.users.len()
    }

    // Note: This method is not idempotent, and in particular, when adding the same user twice,
    // it will completely mess up the indexes.
    pub fn add_user(&mut self, email: &spark::EmailRef) -> &mut User {
        let user_pos = self.users.len();
        self.users.push(User::new(email.to_owned()));
        self.email_index.insert(email.to_owned(), user_pos);
        self.users.last_mut().unwrap()
    }

    fn find_or_add_user_by_email(&mut self, email: &spark::EmailRef) -> &mut User {
        let pos = self.users.iter().position(|u| u.email() == email);
        let user: &mut User = match pos {
            Some(pos) => &mut self.users[pos],
            None => self.add_user(email),
        };
        user
    }

    fn find_user_mut<P: ?Sized>(&mut self, email: &P) -> Option<&mut User>
    where
        spark::Email: std::borrow::Borrow<P>,
        P: std::hash::Hash + Eq,
    {
        self.email_index
            .get(email)
            .cloned()
            .map(move |pos| &mut self.users[pos])
    }

    pub fn find_user<P: ?Sized>(&self, email: &P) -> Option<&User>
    where
        spark::Email: std::borrow::Borrow<P>,
        P: std::hash::Hash + Eq,
    {
        self.email_index
            .get(email)
            .cloned()
            .map(|pos| &self.users[pos])
    }

    pub fn reset_flags(&mut self, email: &spark::EmailRef) -> &User {
        let user = self.find_or_add_user_by_email(email);
        user.reset_flags();
        user
    }

    pub fn set_flag(&mut self, email: &spark::EmailRef, flag: UserFlag, value: bool) -> &User {
        let user = self.find_or_add_user_by_email(email);
        user.set_flag(flag, value);
        user
    }

    pub fn enable<'a>(&'a mut self, email: &spark::EmailRef, enabled: bool) -> &'a User {
        let user: &'a mut User = self.find_or_add_user_by_email(email);
        user.set_enabled(enabled);
        user
    }

    pub fn add_filter(
        &mut self,
        email: &spark::EmailRef,
        filter: &str,
    ) -> Result<(), regex::Error> {
        let user = self.find_or_add_user_by_email(email);
        user.set_filter(Filter {
            regex: Regex::new(filter)?,
            enabled: true,
        });
        Ok(())
    }

    /// Get the filter for the given user given the user exists and has a filter
    /// configured.
    pub fn get_filter(&self, email: &spark::EmailRef) -> Option<(&str, bool)> {
        self.find_user(email)
            .and_then(|u| u.filter())
            .map(|f| (f.regex.as_str(), f.enabled))
    }

    /// Enable or disable the configured filter for the user and return it given
    /// that the user exists and has a filter configured. `None` means the user
    /// doesn't exist or doesn't have a filter configured.
    pub fn enable_and_get_filter(
        &mut self,
        email: &spark::EmailRef,
        enabled: bool,
    ) -> Option<&str> {
        self.find_user_mut(email)
            .and_then(|u| {
                u.set_filter_enabled(enabled);
                u.filter()
            })
            .map(|f| f.regex.as_str())
    }

    pub fn users(&self) -> impl Iterator<Item = &User> + Clone {
        self.users.iter()
    }

    pub fn is_filtered(&self, user: &User, msg: &str) -> bool {
        user.filter()
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
        assert_eq!(state.users[0].email(), EmailRef::new("some@example.com"));
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
        assert_eq!(state.users[1].email(), EmailRef::new("some_2@example.com"));
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
        assert_eq!(user.unwrap().email(), EmailRef::new("some@example.com"));

        let user = state.find_user(EmailRef::new("some_2@example.com"));
        assert!(user.is_some());
        assert_eq!(user.unwrap().email(), EmailRef::new("some_2@example.com"));
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
            .any(|u| u.email() == EmailRef::new("some@example.com") && u.filter().is_none()));

        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), true);
        assert!(res.is_none());
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), false);
        assert!(res.is_none());
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
            .any(|u| u.email() == EmailRef::new("some@example.com")
                && u.filter().map(|f| f.regex.as_str()) == Some(".*some_word.*")));

        {
            let filter = state.get_filter(EmailRef::new("some@example.com"));
            assert_eq!(filter, Some((".*some_word.*", true)));
        }
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), false);
        assert_eq!(res, Some(".*some_word.*"));
        assert!(state
            .users
            .iter()
            .any(|u| u.email() == EmailRef::new("some@example.com")
                && u.filter().map(|f| f.enabled) == Some(false)));
        {
            let filter = state.get_filter(EmailRef::new("some@example.com"));
            assert_eq!(filter, Some((".*some_word.*", false)));
        }
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), true);
        assert_eq!(res, Some(".*some_word.*"));
        assert!(state
            .users
            .iter()
            .any(|u| u.email() == EmailRef::new("some@example.com")
                && u.filter().map(|f| f.enabled) == Some(true)));
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
        assert_eq!(res, Some(".*some_word.*"));
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), false);
        assert_eq!(res, Some(".*some_word.*"));
    }

    #[test]
    fn add_valid_filter_for_disabled_user() {
        let mut state = State::new();
        state.add_user(EmailRef::new("some@example.com"));
        state.users[0].set_enabled(false);

        let res = state.add_filter(EmailRef::new("some@example.com"), ".*some_word.*");
        assert_eq!(res, Ok(()));
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), true);
        assert_eq!(res, Some(".*some_word.*"));
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), false);
        assert_eq!(res, Some(".*some_word.*"));
    }

    #[test]
    fn enable_non_configured_filter_for_existing_user() {
        let mut state = State::new();
        state.add_user(EmailRef::new("some@example.com"));

        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), true);
        assert!(res.is_none());
        let res = state.enable_and_get_filter(EmailRef::new("some@example.com"), false);
        assert!(res.is_none());
    }
}
