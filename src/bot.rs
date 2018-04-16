use std::collections::HashMap;
use std::convert;
use std::fs::File;
use std::io::Read;
use std::io;
use std::path::Path;
use std::time::Duration;

use itertools::Itertools;
use lru_time_cache::LruCache;
use regex::Regex;
use rlua::{Function as LuaFunction, Lua};
use serde_json;

use gerrit;
use spark;

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
struct User {
    spark_person_id: spark::PersonId,
    /// email of the user; assumed to be the same in Spark and Gerrit
    email: spark::Email,
    enabled: bool,
    filter: Option<Filter>,
}

impl User {
    fn new<A: Into<String>>(person_id: A, email: A) -> Self {
        Self {
            spark_person_id: person_id.into(),
            email: email.into(),
            filter: None,
            enabled: true,
        }
    }
}

/// Cache line in LRU Cache containing last approval messages
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum MsgCacheLine {
    Approval {
        /// position of the user in bots.user vector
        user_ref: usize,
        subject: String,
        approver: String,
        approval_type: String,
        approval_value: String,
    },
    ReviewerAdded {
        user_ref: usize,
        subject: String,
    },
}

impl MsgCacheLine {
    fn new_approval(
        user_ref: usize,
        subject: String,
        approver: String,
        approval_type: String,
        approval_value: String,
    ) -> MsgCacheLine {
        MsgCacheLine::Approval {
            user_ref: user_ref,
            subject: subject,
            approver: approver,
            approval_type: approval_type,
            approval_value: approval_value,
        }
    }

    fn new_reviewer_added(user_ref: usize, subject: String) -> MsgCacheLine {
        MsgCacheLine::ReviewerAdded {
            user_ref: user_ref,
            subject: subject,
        }
    }
}

/// Describes a state of the bot
#[derive(Clone, Serialize, Deserialize)]
pub struct Bot {
    users: Vec<User>,
    /// We use Option<Cache> here, to be able to create an empty bot without initializing the
    /// cache.
    #[serde(skip_serializing, skip_deserializing)]
    msg_cache: Option<LruCache<MsgCacheLine, ()>>,
    #[serde(skip_serializing, skip_deserializing)]
    person_id_index: HashMap<String, usize>,
    #[serde(skip_serializing, skip_deserializing)]
    email_index: HashMap<String, usize>,
}

#[derive(Debug)]
pub enum BotError {
    Io(io::Error),
    Serialization(serde_json::Error),
}

impl convert::From<io::Error> for BotError {
    fn from(err: io::Error) -> BotError {
        BotError::Io(err)
    }
}

impl convert::From<serde_json::Error> for BotError {
    fn from(err: serde_json::Error) -> BotError {
        BotError::Serialization(err)
    }
}

#[derive(Debug, PartialEq)]
pub enum AddFilterResult {
    UserNotFound,
    UserDisabled,
    InvalidFilter,
    FilterNotConfigured,
}

impl Bot {
    pub fn new() -> Bot {
        Bot {
            users: Vec::new(),
            msg_cache: None,
            person_id_index: HashMap::new(),
            email_index: HashMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_msg_cache(capacity: usize, expiration: Duration) -> Bot {
        Bot {
            users: Vec::new(),
            msg_cache: Some(
                LruCache::<MsgCacheLine, ()>::with_expiry_duration_and_capacity(
                    expiration,
                    capacity,
                ),
            ),
            person_id_index: HashMap::new(),
            email_index: HashMap::new(),
        }
    }

    pub fn init_msg_cache(&mut self, capacity: usize, expiration: Duration) {
        self.msg_cache = Some(
            LruCache::<MsgCacheLine, ()>::with_expiry_duration_and_capacity(expiration, capacity),
        );
    }

    fn index_users(&mut self) {
        for (user_pos, user) in self.users.iter().enumerate() {
            self.person_id_index
                .insert(user.spark_person_id.clone(), user_pos);
            self.email_index.insert(user.email.clone(), user_pos);
        }
    }

    // Note: This method is not idempotent, and in particular, when adding the same user twice,
    // it will completely mess up the indexes.
    fn add_user<'a>(&'a mut self, person_id: &str, email: &str) -> &'a mut User {
        let user_pos = self.users.len();
        self.users.push(User::new(person_id, email));
        self.person_id_index.insert(person_id.into(), user_pos);
        self.email_index.insert(email.into(), user_pos);
        self.users.last_mut().unwrap()
    }

    fn find_or_add_user_by_person_id<'a>(
        &'a mut self,
        person_id: &str,
        email: &str,
    ) -> &'a mut User {
        let pos = self.users
            .iter()
            .position(|u| u.spark_person_id == person_id);
        let user: &'a mut User = match pos {
            Some(pos) => &mut self.users[pos],
            None => self.add_user(person_id, email),
        };
        user
    }

    fn find_user_mut<'a>(&'a mut self, person_id: &str) -> Option<&'a mut User> {
        self.person_id_index
            .get(person_id)
            .cloned()
            .map(move |pos| &mut self.users[pos])
    }

    fn find_user<'a>(&'a self, person_id: &str) -> Option<&'a User> {
        self.person_id_index
            .get(person_id)
            .cloned()
            .map(|pos| &self.users[pos])
    }

    fn touch_cache(&mut self, key: MsgCacheLine) -> bool {
        if let Some(cache) = self.msg_cache.as_mut() {
            let hit = cache.get(&key).is_some();
            if hit {
                return true;
            } else {
                cache.insert(key, ());
                return false;
            }
        };
        false
    }

    fn enable<'a>(&'a mut self, person_id: &str, email: &str, enabled: bool) -> &'a User {
        let user: &'a mut User = self.find_or_add_user_by_person_id(person_id, email);
        user.enabled = enabled;
        user
    }

    fn format_msg(event: &gerrit::Event, approval: &gerrit::Approval, is_human: bool) -> String {
        let filename = String::from("scripts/format.lua");
        let mut script = String::new();
        File::open(&Path::new(&filename))
            .unwrap()
            .read_to_string(&mut script)
            .unwrap();

        let lua = Lua::new();
        let globals = lua.globals();
        lua.eval::<()>(&script, None).unwrap();
        let f: LuaFunction = globals.get("main").unwrap();
        let lua_event = lua.create_table().unwrap();

        lua_event
            .set("approver", event.author.as_ref().unwrap().username.clone())
            .unwrap();
        lua_event.set("comment", event.comment.clone()).unwrap();
        lua_event
            .set("value", approval.value.parse().unwrap_or(0))
            .unwrap();
        lua_event
            .set("type", approval.approval_type.clone())
            .unwrap();
        lua_event.set("url", event.change.url.clone()).unwrap();
        lua_event
            .set("subject", event.change.subject.clone())
            .unwrap();
        lua_event
            .set("project", event.change.project.clone())
            .unwrap();
        lua_event
            .set("is_human", is_human.clone())
            .unwrap();

        f.call::<_, String>(lua_event).unwrap()
    }

    fn get_approvals_msg(&mut self, event: &gerrit::Event) -> Option<(&User, String, bool)> {
        debug!("Incoming approvals: {:#?}", event);

        if event.approvals.is_none() {
            return None;
        }

        let approvals = tryopt![event.approvals.as_ref()];
        let change = &event.change;
        let approver = &tryopt![event.author.as_ref()].username;
        if approver == &change.owner.username {
            // No need to notify about user's own approvals.
            return None;
        }
        let owner_email = tryopt![change.owner.email.as_ref()];

        // try to find the use and check it is enabled
        let user_pos = *tryopt![self.email_index.get(owner_email)];
        if !self.users[user_pos].enabled {
            return None;
        }

        let is_human = !approver.to_lowercase().contains("bot");

        let msgs: Vec<String> = approvals
            .iter()
            .filter_map(|approval| {
                // filter all messages that were already sent to the user recently
                if self.touch_cache(MsgCacheLine::new_approval(
                    user_pos,
                    if change.topic.is_some() {
                        change.topic.as_ref().unwrap().clone()
                    } else {
                        change.subject.clone()
                    },
                    approver.clone(),
                    approval.approval_type.clone(),
                    approval.value.clone(),
                )) {
                    debug!("Filtered approval due to cache hit.");
                    return None;
                }

                let msg = Self::format_msg(event, approval, is_human);
                // if user has configured and enabled a filter try to apply it
                if self.is_filtered(user_pos, &msg) {
                    return None;
                }

                if !msg.is_empty() {
                    Some(msg)
                } else {
                    None
                }
            })
            .collect();

        if !msgs.is_empty() {
            Some((&self.users[user_pos], msgs.join("\n\n"), is_human)) // two newlines since it is markdown
        } else {
            None
        }
    }

    fn get_reviewer_added_msg(&mut self, event: &gerrit::Event) -> Option<(&User, String)> {
        let reviewer = event.reviewer.as_ref()?.clone();
        let reviewer_email = reviewer.email.as_ref()?;
        let user_pos = *self.email_index.get(reviewer_email)?;
        if !self.users[user_pos].enabled {
            return None;
        }
        let change = &event.change;

        // filter all messages that were already sent to the user recently
        if self.touch_cache(MsgCacheLine::new_reviewer_added(
            user_pos,
            if change.topic.is_some() {
                change.topic.as_ref().unwrap().clone()
            } else {
                change.subject.clone()
            },
        )) {
            debug!("Filtered reviewer-added due to cache hit.");
            return None;
        }

        Some((
            &self.users[user_pos],
            format!(
                "[{}]({}) ({}) 👓 Added as reviewer",
                event.change.subject, event.change.url, event.change.owner.username
            ),
        ))
    }

    pub fn save<P>(self, filename: P) -> Result<(), BotError>
    where
        P: AsRef<Path>,
    {
        let f = File::create(filename)?;
        serde_json::to_writer(f, &self)?;
        Ok(())
    }

    pub fn load<P>(filename: P) -> Result<Self, BotError>
    where
        P: AsRef<Path>,
    {
        let f = File::open(filename)?;
        let mut bot: Bot = serde_json::from_reader(f)?;
        bot.index_users();
        Ok(bot)
    }

    pub fn num_users(&self) -> usize {
        self.users.len()
    }

    pub fn status_for(&self, person_id: &str) -> String {
        let user = self.find_user(person_id);
        let enabled = user.map_or(false, |u| u.enabled);
        format!(
            "Notifications for you are **{}**. I am notifying another {} user(s).",
            if enabled { "enabled" } else { "disabled" },
            if self.num_users() == 0 {
                0
            } else {
                self.num_users() - if enabled { 1 } else { 0 }
            }
        )
    }

    pub fn add_filter<A>(&mut self, person_id: &str, filter: A) -> Result<(), AddFilterResult>
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
        person_id: &str,
    ) -> Result<Option<&'a Filter>, AddFilterResult> {
        let user = self.find_user(person_id);
        match user {
            Some(user) => Ok(user.filter.as_ref()),
            None => Err(AddFilterResult::UserNotFound),
        }
    }

    pub fn enable_filter(
        &mut self,
        person_id: &str,
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

    fn is_filtered(&self, user_pos: usize, msg: &str) -> bool {
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

#[derive(Debug)]
pub enum Action {
    Enable(spark::PersonId, spark::Email),
    Disable(spark::PersonId, spark::Email),
    UpdateApprovals(Box<gerrit::Event>),
    Help(spark::PersonId),
    Unknown(spark::PersonId),
    Status(spark::PersonId),
    FilterStatus(spark::PersonId),
    FilterAdd(spark::PersonId, String /* filter */),
    FilterEnable(spark::PersonId),
    FilterDisable(spark::PersonId),
    ReviewerAdded(Box<gerrit::Event>),
    ChangeFetched(
        spark::PersonId,
        String, /* message */
        Box<gerrit::Change>,
    ),
    NoOp,
}

#[derive(Debug)]
pub struct Response {
    pub person_id: spark::PersonId,
    pub message: String,
}

impl Response {
    pub fn new<A>(person_id: spark::PersonId, message: A) -> Response
    where
        A: Into<String>,
    {
        Response {
            person_id: person_id,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub enum Task {
    Reply(Response),
    ReplyAndSave(Response),
    FetchComments(String, String, String),
}

const GREETINGS_MSG: &str =
r#"Hi. I am GerritBot (**in a beta phase**). I can watch Gerrit reviews for you, and notify you about new +1/-1's.

To enable notifications, just type in **enable**. A small note: your email in Spark and in Gerrit has to be the same. Otherwise, I can't match your accounts.

For more information, type in **help**.

By the way, my icon is made by
[ Madebyoliver ](http://www.flaticon.com/authors/madebyoliver)
from
[ www.flaticon.com ](http://www.flaticon.com)
and is licensed by
[ CC 3.0 BY](http://creativecommons.org/licenses/by/3.0/).
"#;

const HELP_MSG: &str = r#"Commands:

`enable` -- I will start notifying you.

`disable` -- I will stop notifying you.

`filter <regex>` -- Filter all messages by applying the specified regex pattern. If the pattern matches, the message is filtered. The pattern is applied to the full text I send to you. Be aware, to send this command **not** in markdown mode, otherwise, Spark would eat some special characters in the pattern. For regex specification, cf. https://doc.rust-lang.org/regex/regex/index.html#syntax.

`filter enable` -- Enable the filtering of messages with the configured filter.

`filter disable` -- Disable the filtering of messages with the configured filter.

`status` -- Show if I am notifying you, and a little bit more information. 😉

`help` -- This message
"#;

/// Action controller
/// Return new bot state and an optional message to send to the user
pub fn update(action: Action, bot: Bot) -> (Bot, Option<Task>) {
    let mut bot = bot;
    let task = match action {
        Action::Enable(person_id, email) => {
            bot.enable(&person_id, &email, true);
            let task = Task::ReplyAndSave(Response::new(person_id, "Got it! Happy reviewing!"));
            Some(task)
        }
        Action::Disable(person_id, email) => {
            bot.enable(&person_id, &email, false);
            let task = Task::ReplyAndSave(Response::new(person_id, "Got it! I will stay silent."));
            Some(task)
        }
        Action::UpdateApprovals(event) => {
            bot.get_approvals_msg(&event).map(|(user, message, is_human)| {
                if is_human {
                    Task::FetchComments(user.spark_person_id.clone(), event.change.id, message)
                } else {
                    Task::Reply(Response::new(user.spark_person_id.clone(), message))
                }
            })
        }
        Action::Help(person_id) => Some(Task::Reply(Response::new(person_id, HELP_MSG))),
        Action::Unknown(person_id) => Some(Task::Reply(Response::new(person_id, GREETINGS_MSG))),
        Action::Status(person_id) => {
            let status = bot.status_for(&person_id);
            Some(Task::Reply(Response::new(person_id, status)))
        }
        Action::FilterStatus(person_id) => {
            let resp: String = match bot.get_filter(&person_id) {
                Ok(Some(filter)) => {
                    format!(
                        "The following filter is configured for you: `{}`. It is **{}**.",
                        filter.regex,
                        if filter.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    )
                }
                Ok(None) => "No filter is configured for you.".into(),
                Err(err) => {
                    match err {
                        AddFilterResult::UserNotFound => {
                            "Notification for you are disabled. Please enable notifications first, and then add a filter.".into()
                        }
                        _ => {
                            error!("Invalid action arm with Error: {:?}", err);
                            "".into()
                        }
                    }
                }
            };
            if !resp.is_empty() {
                Some(Task::Reply(Response::new(person_id, resp)))
            } else {
                None
            }
        }
        Action::FilterAdd(person_id, filter) => {
            Some(match bot.add_filter(&person_id, filter) {
                Ok(()) => Task::ReplyAndSave(Response::new(
                    person_id,
                    "Filter successfully added and enabled.")),
                Err(err) => {
                    Task::Reply(Response::new(
                        person_id,
                        match err {
                            AddFilterResult::UserDisabled |
                            AddFilterResult::UserNotFound => {
                                "Notification for you are disabled. Please enable notifications first, and then add a filter."
                            }
                            AddFilterResult::InvalidFilter => {
                                "Your provided filter is invalid. Please double-check the regex you provided. Specifications of the regex are here: https://doc.rust-lang.org/regex/regex/index.html#syntax"
                            }
                            AddFilterResult::FilterNotConfigured => {
                                assert!(false, "this should not be possible");
                                ""
                            }
                        },
                    ))
                }
            })
        }
        Action::FilterEnable(person_id) => {
            Some(match bot.enable_filter(&person_id, true) {
                Ok(filter) => {
                    Task::ReplyAndSave(Response::new(
                        person_id,
                        format!(
                            "Filter successfully enabled. The following filter is configured: {}",
                            filter
                        ),
                    ))
                }
                Err(err) => {
                    Task::Reply(Response::new(
                        person_id,
                        match err {
                            AddFilterResult::UserDisabled |
                            AddFilterResult::UserNotFound => {
                                "Notification for you are disabled. Please enable notifications first, and then add a filter."
                            }
                            AddFilterResult::InvalidFilter => {
                                "Your provided filter is invalid. Please double-check the regex you provided. Specifications of the regex are here: https://doc.rust-lang.org/regex/regex/index.html#syntax"
                            }
                            AddFilterResult::FilterNotConfigured => {
                                "Cannot enable filter since there is none configured. User `filter <regex>` to add a new filter."
                            }
                        },
                    ))
                }
            })
        }
        Action::FilterDisable(person_id) => {
            Some(match bot.enable_filter(&person_id, false) {
                Ok(_) => Task::ReplyAndSave(
                    Response::new(person_id, "Filter successfully disabled."),
                ),
                Err(err) => {
                    Task::Reply(Response::new(
                        person_id,
                        match err {
                            AddFilterResult::UserDisabled |
                            AddFilterResult::UserNotFound => {
                                "Notification for you are disabled. No need to disable the filter."
                            }
                            AddFilterResult::InvalidFilter => {
                                "Your provided filter is invalid. Please double-check the regex you provided. Specifications of the regex are here: https://doc.rust-lang.org/regex/regex/index.html#syntax"
                            }
                            AddFilterResult::FilterNotConfigured => {
                                "No need to disable the filter since there is none configured."
                            }
                        },
                    ))
                }
            })
        }
        Action::ReviewerAdded(event) => {
            bot.get_reviewer_added_msg(&event).map(|(user, message)| {
                Task::Reply(Response::new(user.spark_person_id.clone(), message))
            })
        }
        Action::ChangeFetched(person_id, message, change) => {
            Some(Task::Reply(Response::new(
                person_id, format_msg_with_comments(message, *change))))
        }
        Action::NoOp => None,
    };
    (bot, task)
}

fn format_comments(change: gerrit::Change) -> Option<String> {
    let change_number = change.number.clone();
    let host = change.url.split('/').nth(2).unwrap();

    change.current_patch_set.map(|patch_set| {
        let patch_set_number = patch_set.number;
        let mut comments = patch_set.comments.unwrap_or_else(Vec::new);
        comments.sort_by(|a, b| a.file.cmp(&b.file));

        comments
            .into_iter()
            .group_by(|c| c.file.clone())
            .into_iter()
            .map(|(file, comments)| -> String {
                let line_comments = comments
                    .map(|comment| {
                        let url = format!(
                            "https://{}/#/c/{}/{}/{}@{}",
                            host, change_number, patch_set_number, comment.file, comment.line
                        );
                        format!("> [Line {}]({}): {}", comment.line, url, comment.message)
                    })
                    .intersperse("\n".into())
                    .collect::<Vec<_>>()
                    .concat();
                format!("`{}`\n\n{}", file, line_comments)
            })
            .intersperse("\n\n".into())
            .collect::<Vec<_>>()
            .concat()
    })
}

fn format_msg_with_comments(message: String, change: gerrit::Change) -> String {
    if let Some(additional_message) = format_comments(change) {
        format!("{}\n\n{}", message, additional_message)
    } else {
        message
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;
    use std::thread;

    use serde_json;

    use gerrit;
    use super::*;

    #[test]
    fn add_user() {
        let mut bot = Bot::new();
        bot.add_user("some_person_id", "some@example.com");
        assert_eq!(bot.users.len(), 1);
        assert_eq!(bot.users[0].spark_person_id, "some_person_id");
        assert_eq!(bot.users[0].email, "some@example.com");
        assert!(bot.users[0].enabled);
    }

    #[test]
    fn status_for() {
        let mut bot = Bot::new();
        bot.add_user("some_person_id", "some@example.com");

        let resp = bot.status_for("some_person_id");
        assert!(resp.contains("enabled"));

        bot.users[0].enabled = false;
        let resp = bot.status_for("some_person_id");
        assert!(resp.contains("disabled"));

        let resp = bot.status_for("some_non_existent_id");
        assert!(resp.contains("disabled"));
    }

    #[test]
    fn enable_non_existent_user() {
        let test = |enable| {
            let mut bot = Bot::new();
            let num_users = bot.num_users();
            bot.enable("some_person_id", "some@example.com", enable);
            assert!(
                bot.users
                    .iter()
                    .position(|u| u.spark_person_id == "some_person_id"
                        && u.email == "some@example.com"
                        && u.enabled == enable)
                    .is_some()
            );
            assert!(bot.num_users() == num_users + 1);
        };
        test(true);
        test(false);
    }

    #[test]
    fn enable_existent_user() {
        let test = |enable| {
            let mut bot = Bot::new();
            bot.add_user("some_person_id", "some@example.com");
            let num_users = bot.num_users();

            bot.enable("some_person_id", "some@example.com", enable);
            assert!(
                bot.users
                    .iter()
                    .position(|u| u.spark_person_id == "some_person_id"
                        && u.email == "some@example.com"
                        && u.enabled == enable)
                    .is_some()
            );
            assert!(bot.num_users() == num_users);
        };
        test(true);
        test(false);
    }

    const EVENT_JSON : &'static str = r#"
{"author":{"name":"Approver","username":"approver"},"approvals":[{"type":"Code-Review","description":"Code-Review","value":"2","oldValue":"-1"}],"comment":"Patch Set 1: Code-Review+2\n\nJust a buggy script. FAILURE\n\nAnd more problems. FAILURE","patchSet":{"number":"1","revision":"49a65998c02eda928559f2d0b586c20bc8e37b10","parents":["fb1909b4eda306985d2bbce769310e5a50a98cf5"],"ref":"refs/changes/42/42/1","uploader":{"name":"Author","email":"author@example.com","username":"Author"},"createdOn":1494165142,"author":{"name":"Author","email":"author@example.com","username":"Author"},"isDraft":false,"kind":"REWORK","sizeInsertions":0,"sizeDeletions":0},"change":{"project":"demo-project","branch":"master","id":"Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14","number":"49","subject":"Some review.","owner":{"name":"Author","email":"author@example.com","username":"author"},"url":"http://localhost/42","commitMessage":"Some review.\n\nChange-Id: Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14\n","status":"NEW"},"project":"demo-project","refName":"refs/heads/master","changeKey":{"id":"Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14"},"type":"comment-added","eventCreatedOn":1499190282}"#;

    fn get_event() -> gerrit::Event {
        let event: Result<gerrit::Event, _> = serde_json::from_str(EVENT_JSON);
        assert!(event.is_ok());
        event.unwrap()
    }

    #[test]
    fn test_add_user() {
        let mut bot = Bot::new();
        bot.add_user("some_person_id", "some@example.com");
        assert_eq!(bot.users.len(), 1);
        assert_eq!(bot.person_id_index.len(), 1);
        assert_eq!(bot.email_index.len(), 1);
        assert_eq!(bot.users[0].spark_person_id, "some_person_id");
        assert_eq!(bot.users[0].email, "some@example.com");
        assert_eq!(bot.person_id_index.get("some_person_id"), Some(&0));
        assert_eq!(bot.email_index.get("some@example.com"), Some(&0));

        bot.add_user("some_person_id_2", "some_2@example.com");
        assert_eq!(bot.users.len(), 2);
        assert_eq!(bot.person_id_index.len(), 2);
        assert_eq!(bot.email_index.len(), 2);
        assert_eq!(bot.users[1].spark_person_id, "some_person_id_2");
        assert_eq!(bot.users[1].email, "some_2@example.com");
        assert_eq!(bot.person_id_index.get("some_person_id_2"), Some(&1));
        assert_eq!(bot.email_index.get("some_2@example.com"), Some(&1));

        let user = bot.find_user("some_person_id");
        assert!(user.is_some());
        assert_eq!(user.unwrap().spark_person_id, "some_person_id");
        assert_eq!(user.unwrap().email, "some@example.com");

        let user = bot.find_user("some_person_id_2");
        assert!(user.is_some());
        assert_eq!(user.unwrap().spark_person_id, "some_person_id_2");
        assert_eq!(user.unwrap().email, "some_2@example.com");
    }

    #[test]
    fn get_approvals_msg_for_empty_bot() {
        // bot does not have the user => no message
        let mut bot = Bot::new();
        let res = bot.get_approvals_msg(&get_event());
        assert!(res.is_none());
    }

    #[test]
    fn get_approvals_msg_for_same_author_and_approver() {
        // the approval is from the author => no message
        let mut bot = Bot::new();
        bot.add_user("approver_spark_id", "approver@example.com");
        let res = bot.get_approvals_msg(&get_event());
        assert!(res.is_none());
    }

    #[test]
    fn get_approvals_msg_for_user_with_disabled_notifications() {
        // the approval is for the user with disabled notifications
        // => no message
        let mut bot = Bot::new();
        bot.add_user("author_spark_id", "author@example.com");
        bot.users[0].enabled = false;
        let res = bot.get_approvals_msg(&get_event());
        assert!(res.is_none());
    }

    #[test]
    fn get_approvals_msg_for_user_with_enabled_notifications() {
        // the approval is for the user with enabled notifications
        // => message
        let mut bot = Bot::new();
        bot.add_user("author_spark_id", "author@example.com");
        let res = bot.get_approvals_msg(&get_event());
        assert!(res.is_some());
        let (user, msg, is_human) = res.unwrap();
        assert_eq!(user.spark_person_id, "author_spark_id");
        assert_eq!(user.email, "author@example.com");
        assert!(msg.contains("Some review."));
        assert!(is_human);
    }

    #[test]
    fn get_approvals_msg_for_user_with_enabled_notifications_and_filter() {
        // the approval is for the user with enabled notifications
        // => message
        let mut bot = Bot::new();
        bot.add_user("author_spark_id", "author@example.com");

        {
            let res = bot.add_filter("author_spark_id", ".*Code-Review.*");
            assert!(res.is_ok());
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_none());
        }
        {
            let res = bot.enable_filter("author_spark_id", false);
            assert!(res.is_ok());
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, "author_spark_id");
            assert_eq!(user.email, "author@example.com");
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
        {
            let res = bot.enable_filter("author_spark_id", true);
            assert!(res.is_ok());
            let res = bot.add_filter("author_spark_id", "some_non_matching_filter");
            assert!(res.is_ok());
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, "author_spark_id");
            assert_eq!(user.email, "author@example.com");
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
    }

    #[test]
    fn get_approvals_msg_for_quickly_repeated_event() {
        // same approval for the user with enabled notifications 2 times in less than 1 sec
        // => first time get message, second time nothing
        let mut bot = Bot::with_msg_cache(10, Duration::from_secs(1));
        bot.add_user("author_spark_id", "author@example.com");
        {
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, "author_spark_id");
            assert_eq!(user.email, "author@example.com");
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
        {
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_none());
        }
    }

    #[test]
    fn get_approvals_msg_for_slowly_repeated_event() {
        // same approval for the user with enabled notifications 2 times in more than 100 msec
        // => get message 2 times
        let mut bot = Bot::with_msg_cache(10, Duration::from_millis(50));
        bot.add_user("author_spark_id", "author@example.com");
        {
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, "author_spark_id");
            assert_eq!(user.email, "author@example.com");
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
        thread::sleep(Duration::from_millis(200));
        {
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, "author_spark_id");
            assert_eq!(user.email, "author@example.com");
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
    }

    #[test]
    fn get_approvals_msg_for_bot_with_low_msgs_capacity() {
        // same approval for the user with enabled notifications 2 times in more less 100 msec
        // but there is also another approval and bot's msg capacity is 1
        // => get message 3 times
        let mut bot = Bot::with_msg_cache(1, Duration::from_secs(1));
        bot.add_user("author_spark_id", "author@example.com");
        {
            let mut event = get_event();
            event.change.subject = String::from("A");
            let res = bot.get_approvals_msg(&event);
            assert!(res.is_some());
        }
        {
            let mut event = get_event();
            event.change.subject = String::from("B");
            let res = bot.get_approvals_msg(&event);
            assert!(res.is_some());
        }
        {
            let mut event = get_event();
            event.change.subject = String::from("A");
            let res = bot.get_approvals_msg(&event);
            assert!(res.is_some());
        }
    }

    #[test]
    fn test_format_msg() {
        let mut bot = Bot::new();
        bot.add_user("author_spark_id", "author@example.com");
        let event = get_event();
        let res = Bot::format_msg(&event, &event.approvals.as_ref().unwrap()[0], true);
        assert_eq!(
            res,
            "[Some review.](http://localhost/42) (demo-project) 👍 +2 (Code-Review) from approver\n\n> Just a buggy script. FAILURE<br>\n> And more problems. FAILURE"
        );
    }

    #[test]
    fn format_msg_filters_specific_messages() {
        let mut bot = Bot::new();
        bot.add_user("author_spark_id", "author@example.com");
        let mut event = get_event();
        event.approvals.as_mut().unwrap()[0].approval_type = String::from("Some new type");
        let res = Bot::format_msg(&event, &event.approvals.as_ref().unwrap()[0], true);
        assert!(res.is_empty());
    }

    #[test]
    fn add_invalid_filter_for_existing_user() {
        let mut bot = Bot::new();
        bot.add_user("some_person_id", "some@example.com");

        let res = bot.add_filter("some_person_id", ".some_weard_regex/[");
        assert_eq!(res, Err(AddFilterResult::InvalidFilter));
        assert!(
            bot.users
                .iter()
                .position(
                    |u| u.spark_person_id == "some_person_id" && u.email == "some@example.com"
                        && u.filter == None
                )
                .is_some()
        );

        let res = bot.enable_filter("some_person_id", true);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
        let res = bot.enable_filter("some_person_id", false);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
    }

    #[test]
    fn add_valid_filter_for_existing_user() {
        let mut bot = Bot::new();
        bot.add_user("some_person_id", "some@example.com");

        let res = bot.add_filter("some_person_id", ".*some_word.*");
        assert!(res.is_ok());
        assert!(
            bot.users
                .iter()
                .position(
                    |u| u.spark_person_id == "some_person_id" && u.email == "some@example.com"
                        && u.filter == Some(Filter::new(".*some_word.*"))
                )
                .is_some()
        );

        {
            let filter = bot.get_filter("some_person_id");
            assert_eq!(filter, Ok(Some(&Filter::new(".*some_word.*"))));
        }
        let res = bot.enable_filter("some_person_id", false);
        assert_eq!(res, Ok(String::from(".*some_word.*")));
        assert!(
            bot.users
                .iter()
                .position(
                    |u| u.spark_person_id == "some_person_id" && u.email == "some@example.com"
                        && u.filter.as_ref().map(|f| f.enabled) == Some(false)
                )
                .is_some()
        );
        {
            let filter = bot.get_filter("some_person_id").unwrap().unwrap();
            assert_eq!(filter.regex, ".*some_word.*");
            assert_eq!(filter.enabled, false);
        }
        let res = bot.enable_filter("some_person_id", true);
        assert_eq!(res, Ok(String::from(".*some_word.*")));
        assert!(
            bot.users
                .iter()
                .position(
                    |u| u.spark_person_id == "some_person_id" && u.email == "some@example.com"
                        && u.filter.as_ref().map(|f| f.enabled) == Some(true)
                )
                .is_some()
        );
        {
            let filter = bot.get_filter("some_person_id");
            assert_eq!(filter, Ok(Some(&Filter::new(".*some_word.*"))));
        }
    }

    #[test]
    fn add_valid_filter_for_non_existing_user() {
        let mut bot = Bot::new();
        let res = bot.add_filter("some_person_id", ".*some_word.*");
        assert_eq!(res, Err(AddFilterResult::UserNotFound));
        let res = bot.enable_filter("some_person_id", true);
        assert_eq!(res, Err(AddFilterResult::UserNotFound));
        let res = bot.enable_filter("some_person_id", false);
        assert_eq!(res, Err(AddFilterResult::UserNotFound));
    }

    #[test]
    fn add_valid_filter_for_disabled_user() {
        let mut bot = Bot::new();
        bot.add_user("some_person_id", "some@example.com");
        bot.users[0].enabled = false;

        let res = bot.add_filter("some_person_id", ".*some_word.*");
        assert_eq!(res, Err(AddFilterResult::UserDisabled));
        let res = bot.enable_filter("some_person_id", true);
        assert_eq!(res, Err(AddFilterResult::UserDisabled));
        let res = bot.enable_filter("some_person_id", false);
        assert_eq!(res, Err(AddFilterResult::UserDisabled));
    }

    #[test]
    fn enable_non_configured_filter_for_existing_user() {
        let mut bot = Bot::new();
        bot.add_user("some_person_id", "some@example.com");

        let res = bot.enable_filter("some_person_id", true);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
        let res = bot.enable_filter("some_person_id", false);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
    }

    #[test]
    fn enable_invalid_filter_for_existing_user() {
        let mut bot = Bot::new();
        bot.add_user("some_person_id", "some@example.com");
        bot.users[0].filter = Some(Filter::new("invlide_filter_set_from_outside["));

        let res = bot.enable_filter("some_person_id", true);
        assert_eq!(res, Err(AddFilterResult::InvalidFilter));
        let res = bot.enable_filter("some_person_id", false);
        assert_eq!(res, Err(AddFilterResult::InvalidFilter));
    }
}
