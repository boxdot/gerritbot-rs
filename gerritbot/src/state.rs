use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use log::warn;
use regex::Regex;
use serde::{Deserialize, Serialize};

use gerritbot_spark as spark;

use super::BotError;

#[derive(Debug, PartialEq)]
pub enum AddFilterResult {
    UserNotFound,
    UserDisabled,
    InvalidFilter,
    FilterNotConfigured,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Filter {
    pub regex: String,
    pub enabled: bool,
}

impl Filter {
    pub fn new<A: Into<String>>(regex: A) -> Self {
        Self {
            regex: regex.into(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub spark_person_id: spark::PersonId,
    /// email of the user; assumed to be the same in Spark and Gerrit
    pub email: spark::Email,
    pub enabled: bool,
    pub filter: Option<Filter>,
}

impl User {
    fn new(person_id: spark::PersonId, email: spark::Email) -> Self {
        Self {
            spark_person_id: person_id,
            email: email,
            filter: None,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    pub users: Vec<User>,
    #[serde(skip_serializing, skip_deserializing)]
    pub person_id_index: HashMap<spark::PersonId, usize>,
    #[serde(skip_serializing, skip_deserializing)]
    pub email_index: HashMap<spark::Email, usize>,
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
            self.person_id_index
                .insert(user.spark_person_id.clone(), user_pos);
            self.email_index.insert(user.email.clone(), user_pos);
        }
    }

    pub fn num_users(&self) -> usize {
        self.users.len()
    }

    // Note: This method is not idempotent, and in particular, when adding the same user twice,
    // it will completely mess up the indexes.
    pub fn add_user<'a>(
        &'a mut self,
        person_id: &spark::PersonIdRef,
        email: &spark::EmailRef,
    ) -> &'a mut User {
        let user_pos = self.users.len();
        self.users
            .push(User::new(person_id.to_owned(), email.to_owned()));
        self.person_id_index.insert(person_id.to_owned(), user_pos);
        self.email_index.insert(email.to_owned(), user_pos);
        self.users.last_mut().unwrap()
    }

    fn find_or_add_user_by_person_id<'a>(
        &'a mut self,
        person_id: &spark::PersonIdRef,
        email: &spark::EmailRef,
    ) -> &'a mut User {
        let pos = self
            .users
            .iter()
            .position(|u| u.spark_person_id == person_id);
        let user: &'a mut User = match pos {
            Some(pos) => &mut self.users[pos],
            None => self.add_user(person_id, email),
        };
        user
    }

    fn find_user_mut<'a, P: ?Sized>(&'a mut self, person_id: &P) -> Option<&'a mut User>
    where
        spark::PersonId: std::borrow::Borrow<P>,
        P: std::hash::Hash + Eq,
    {
        self.person_id_index
            .get(person_id)
            .cloned()
            .map(move |pos| &mut self.users[pos])
    }

    pub fn find_user<'a, P: ?Sized>(&'a self, person_id: &P) -> Option<&'a User>
    where
        spark::PersonId: std::borrow::Borrow<P>,
        P: std::hash::Hash + Eq,
    {
        self.person_id_index
            .get(person_id)
            .cloned()
            .map(|pos| &self.users[pos])
    }

    pub fn enable<'a>(
        &'a mut self,
        person_id: &spark::PersonIdRef,
        email: &spark::EmailRef,
        enabled: bool,
    ) -> &'a User {
        let user: &'a mut User = self.find_or_add_user_by_person_id(person_id, email);
        user.enabled = enabled;
        user
    }

    pub fn add_filter<A>(
        &mut self,
        person_id: &spark::PersonIdRef,
        filter: A,
    ) -> Result<(), AddFilterResult>
    where
        A: Into<String>,
    {
        let user = self.find_user_mut(person_id);
        match user {
            Some(user) => {
                if !user.enabled {
                    Err(AddFilterResult::UserDisabled)
                } else {
                    let filter: String = filter.into();
                    if Regex::new(&filter).is_err() {
                        return Err(AddFilterResult::InvalidFilter);
                    }
                    user.filter = Some(Filter::new(filter));
                    Ok(())
                }
            }
            None => Err(AddFilterResult::UserNotFound),
        }
    }

    pub fn get_filter<'a>(
        &'a self,
        person_id: &spark::PersonIdRef,
    ) -> Result<Option<&'a Filter>, AddFilterResult> {
        let user = self.find_user(person_id);
        match user {
            Some(user) => Ok(user.filter.as_ref()),
            None => Err(AddFilterResult::UserNotFound),
        }
    }

    pub fn enable_filter(
        &mut self,
        person_id: &spark::PersonIdRef,
        enabled: bool,
    ) -> Result<String /* filter */, AddFilterResult> {
        let user = self.find_user_mut(person_id);
        match user {
            Some(user) => {
                if !user.enabled {
                    Err(AddFilterResult::UserDisabled)
                } else {
                    match user.filter.as_mut() {
                        Some(filter) => {
                            if Regex::new(&filter.regex).is_err() {
                                return Err(AddFilterResult::InvalidFilter);
                            }
                            filter.enabled = enabled;
                            Ok(filter.regex.clone())
                        }
                        None => Err(AddFilterResult::FilterNotConfigured),
                    }
                }
            }
            None => Err(AddFilterResult::UserNotFound),
        }
    }

    pub fn is_filtered(&self, user_pos: usize, msg: &str) -> bool {
        let user = &self.users[user_pos];
        if let Some(filter) = user.filter.as_ref() {
            if filter.enabled {
                if let Ok(re) = Regex::new(&filter.regex) {
                    return re.is_match(msg);
                } else {
                    warn!(
                        "User {} has configured invalid filter regex: {}",
                        user.spark_person_id, filter.regex
                    );
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod test {
    use spark::{EmailRef, PersonIdRef};

    use super::*;

    #[test]
    fn test_add_user() {
        let mut state = State::new();
        state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );
        assert_eq!(state.users.len(), 1);
        assert_eq!(state.person_id_index.len(), 1);
        assert_eq!(state.email_index.len(), 1);
        assert_eq!(
            state.users[0].spark_person_id,
            PersonIdRef::new("some_person_id")
        );
        assert_eq!(state.users[0].email, EmailRef::new("some@example.com"));
        assert_eq!(
            state
                .person_id_index
                .get(PersonIdRef::new("some_person_id")),
            Some(&0)
        );
        assert_eq!(
            state.email_index.get(EmailRef::new("some@example.com")),
            Some(&0)
        );

        state.add_user(
            PersonIdRef::new("some_person_id_2"),
            EmailRef::new("some_2@example.com"),
        );
        assert_eq!(state.users.len(), 2);
        assert_eq!(state.person_id_index.len(), 2);
        assert_eq!(state.email_index.len(), 2);
        assert_eq!(
            state.users[1].spark_person_id,
            PersonIdRef::new("some_person_id_2")
        );
        assert_eq!(state.users[1].email, EmailRef::new("some_2@example.com"));
        assert_eq!(
            state
                .person_id_index
                .get(PersonIdRef::new("some_person_id_2")),
            Some(&1)
        );
        assert_eq!(
            state.email_index.get(EmailRef::new("some_2@example.com")),
            Some(&1)
        );

        let user = state.find_user(PersonIdRef::new("some_person_id"));
        assert!(user.is_some());
        assert_eq!(
            user.unwrap().spark_person_id,
            PersonIdRef::new("some_person_id")
        );
        assert_eq!(user.unwrap().email, EmailRef::new("some@example.com"));

        let user = state.find_user(PersonIdRef::new("some_person_id_2"));
        assert!(user.is_some());
        assert_eq!(
            user.unwrap().spark_person_id,
            PersonIdRef::new("some_person_id_2")
        );
        assert_eq!(user.unwrap().email, EmailRef::new("some_2@example.com"));
    }

    #[test]
    fn add_invalid_filter_for_existing_user() {
        let mut state = State::new();
        state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );
        let res = state.add_filter(PersonIdRef::new("some_person_id"), ".some_weard_regex/[");
        assert_eq!(res, Err(AddFilterResult::InvalidFilter));
        assert!(state
            .users
            .iter()
            .position(|u| u.spark_person_id == PersonIdRef::new("some_person_id")
                && u.email == EmailRef::new("some@example.com")
                && u.filter == None)
            .is_some());

        let res = state.enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
        let res = state.enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
    }

    #[test]
    fn add_valid_filter_for_existing_user() {
        let mut state = State::new();
        state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );

        let res = state.add_filter(PersonIdRef::new("some_person_id"), ".*some_word.*");
        assert!(res.is_ok());
        assert!(state
            .users
            .iter()
            .position(|u| u.spark_person_id == PersonIdRef::new("some_person_id")
                && u.email == EmailRef::new("some@example.com")
                && u.filter == Some(Filter::new(".*some_word.*")))
            .is_some());

        {
            let filter = state.get_filter(PersonIdRef::new("some_person_id"));
            assert_eq!(filter, Ok(Some(&Filter::new(".*some_word.*"))));
        }
        let res = state.enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Ok(String::from(".*some_word.*")));
        assert!(state
            .users
            .iter()
            .position(|u| u.spark_person_id == PersonIdRef::new("some_person_id")
                && u.email == EmailRef::new("some@example.com")
                && u.filter.as_ref().map(|f| f.enabled) == Some(false))
            .is_some());
        {
            let filter = state
                .get_filter(PersonIdRef::new("some_person_id"))
                .unwrap()
                .unwrap();
            assert_eq!(filter.regex, ".*some_word.*");
            assert_eq!(filter.enabled, false);
        }
        let res = state.enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Ok(String::from(".*some_word.*")));
        assert!(state
            .users
            .iter()
            .position(|u| u.spark_person_id == PersonIdRef::new("some_person_id")
                && u.email == EmailRef::new("some@example.com")
                && u.filter.as_ref().map(|f| f.enabled) == Some(true))
            .is_some());
        {
            let filter = state.get_filter(PersonIdRef::new("some_person_id"));
            assert_eq!(filter, Ok(Some(&Filter::new(".*some_word.*"))));
        }
    }

    #[test]
    fn add_valid_filter_for_non_existing_user() {
        let mut state = State::new();
        let res = state.add_filter(PersonIdRef::new("some_person_id"), ".*some_word.*");
        assert_eq!(res, Err(AddFilterResult::UserNotFound));
        let res = state.enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Err(AddFilterResult::UserNotFound));
        let res = state.enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Err(AddFilterResult::UserNotFound));
    }

    #[test]
    fn add_valid_filter_for_disabled_user() {
        let mut state = State::new();
        state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );
        state.users[0].enabled = false;

        let res = state.add_filter(PersonIdRef::new("some_person_id"), ".*some_word.*");
        assert_eq!(res, Err(AddFilterResult::UserDisabled));
        let res = state.enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Err(AddFilterResult::UserDisabled));
        let res = state.enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Err(AddFilterResult::UserDisabled));
    }

    #[test]
    fn enable_non_configured_filter_for_existing_user() {
        let mut state = State::new();
        state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );

        let res = state.enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
        let res = state.enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
    }

    #[test]
    fn enable_invalid_filter_for_existing_user() {
        let mut state = State::new();
        state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );
        state.users[0].filter = Some(Filter::new("invlide_filter_set_from_outside["));

        let res = state.enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Err(AddFilterResult::InvalidFilter));
        let res = state.enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Err(AddFilterResult::InvalidFilter));
    }

}