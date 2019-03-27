use std::borrow::Cow;
use std::collections::HashMap;
use std::convert;
use std::fs::File;
use std::io;
use std::path::Path;
use std::time::Duration;

use futures::{future::Future, stream::Stream};
use itertools::Itertools;
use lazy_static::lazy_static;
use log::{debug, error, warn};
use lru_time_cache::LruCache;
use regex::Regex;
use rlua::{Function as LuaFunction, Lua};
use serde::{Deserialize, Serialize};

use gerritbot_gerrit as gerrit;
use gerritbot_spark as spark;

pub mod args;

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
    fn new(person_id: spark::PersonId, email: spark::Email) -> Self {
        Self {
            spark_person_id: person_id,
            email: email,
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
            user_ref,
            subject,
            approver,
            approval_type,
            approval_value,
        }
    }

    fn new_reviewer_added(user_ref: usize, subject: String) -> MsgCacheLine {
        MsgCacheLine::ReviewerAdded { user_ref, subject }
    }
}

pub trait GerritCommandRunner {}

impl GerritCommandRunner for gerrit::CommandRunner {}

pub trait SparkClient: Clone {
    type ReplyFuture: Future<Item = (), Error = spark::Error> + Send;
    fn reply(&self, person_id: &spark::PersonId, msg: &str) -> Self::ReplyFuture;
}

impl SparkClient for spark::Client {
    type ReplyFuture = Box<dyn Future<Item = (), Error = spark::Error> + Send>;
    fn reply(&self, person_id: &spark::PersonId, msg: &str) -> Self::ReplyFuture {
        Box::new(self.reply(person_id, msg))
    }
}

pub struct Bot<G = gerrit::CommandRunner, S = spark::Client> {
    state: State,
    msg_cache: Option<LruCache<MsgCacheLine, ()>>,
    format_script: String,
    gerrit_command_runner: G,
    spark_client: S,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    users: Vec<User>,
    #[serde(skip_serializing, skip_deserializing)]
    person_id_index: HashMap<spark::PersonId, usize>,
    #[serde(skip_serializing, skip_deserializing)]
    email_index: HashMap<spark::Email, usize>,
}

#[derive(Debug, Clone, Copy)]
struct MsgCacheParameters {
    capacity: usize,
    expiration: Duration,
}

#[derive(Default, Debug, Clone)]
pub struct Builder {
    state: State,
    msg_cache_parameters: Option<MsgCacheParameters>,
    format_script: String,
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

fn get_default_format_script() -> &'static str {
    const DEFAULT_FORMAT_SCRIPT: &str = include_str!("../../scripts/format.lua");
    check_format_script_syntax(DEFAULT_FORMAT_SCRIPT)
        .unwrap_or_else(|err| panic!("invalid format script: {}", err));
    DEFAULT_FORMAT_SCRIPT
}

fn check_format_script_syntax(script_source: &str) -> Result<(), String> {
    let lua = Lua::new();
    lua.context(|context| {
        let globals = context.globals();
        context
            .load(script_source)
            .exec()
            .map_err(|err| format!("syntax error: {}", err))?;
        let _: LuaFunction = globals.get("main").map_err(|_| "main function missing")?;
        Ok(())
    })
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
    fn add_user<'a>(
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

    fn find_user<'a, P: ?Sized>(&'a self, person_id: &P) -> Option<&'a User>
    where
        spark::PersonId: std::borrow::Borrow<P>,
        P: std::hash::Hash + Eq,
    {
        self.person_id_index
            .get(person_id)
            .cloned()
            .map(|pos| &self.users[pos])
    }

    fn enable<'a>(
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

impl Builder {
    pub fn new(state: State) -> Self {
        Self {
            state,
            format_script: get_default_format_script().to_string(),
            ..Default::default()
        }
    }

    pub fn with_msg_cache(self, capacity: usize, expiration: Duration) -> Self {
        Self {
            msg_cache_parameters: Some(MsgCacheParameters {
                capacity,
                expiration,
            }),
            ..self
        }
    }

    pub fn with_format_script(self, script_source: String) -> Result<Self, String> {
        check_format_script_syntax(&script_source)?;
        Ok(Self {
            format_script: script_source,
            ..self
        })
    }

    pub fn build<G, S>(self, gerrit_command_runner: G, spark_client: S) -> Bot<G, S> {
        let Self {
            format_script,
            msg_cache_parameters,
            state,
        } = self;

        Bot {
            gerrit_command_runner,
            spark_client,
            msg_cache: msg_cache_parameters.map(
                |MsgCacheParameters {
                     capacity,
                     expiration,
                 }| {
                    LruCache::<MsgCacheLine, ()>::with_expiry_duration_and_capacity(
                        expiration, capacity,
                    )
                },
            ),
            format_script,
            state,
        }
    }
}

fn spark_message_to_action(message: spark::Message) -> Action {
    lazy_static! {
        static ref FILTER_REGEX: Regex = Regex::new(r"(?i)^filter (.*)$").unwrap();
    };

    let sender_email = message.person_email;
    let sender_id = message.person_id;
    match &message.text.trim().to_lowercase()[..] {
        "enable" => Action::Enable(sender_id, sender_email),
        "disable" => Action::Disable(sender_id, sender_email),
        "status" => Action::Status(sender_id),
        "help" => Action::Help(sender_id),
        "version" => Action::Version(sender_id),
        "filter" => Action::FilterStatus(sender_id),
        "filter enable" => Action::FilterEnable(sender_id),
        "filter disable" => Action::FilterDisable(sender_id),
        _ => FILTER_REGEX
            .captures(&message.text.trim()[..])
            .and_then(|cap| cap.get(1))
            .map(|m| Action::FilterAdd(sender_id.clone(), m.as_str().to_string()))
            .unwrap_or_else(|| Action::Unknown(sender_id.clone())),
    }
}

/// Transform a gerrit event into a bot action.
pub fn gerrit_event_to_action(event: gerrit::Event) -> Option<Action> {
    if event.event_type == gerrit::EventType::CommentAdded && event.approvals.is_some() {
        Some(Action::UpdateApprovals(Box::new(event)))
    } else if event.event_type == gerrit::EventType::ReviewerAdded {
        Some(Action::ReviewerAdded(Box::new(event)))
    } else {
        None
    }
}

pub fn request_extended_gerrit_info(event: &gerrit::Event) -> Cow<'static, [gerrit::ExtendedInfo]> {
    let owner = &event.change.owner.username;
    let approver = event.author.as_ref().map(|user| &user.username);
    let is_human = approver
        .map(|name| !name.to_lowercase().contains("bot"))
        .unwrap_or(false);

    if Some(owner) != approver && is_human && maybe_has_inline_comments(event) {
        Cow::Borrowed(&[gerrit::ExtendedInfo::InlineComments])
    } else {
        Cow::Borrowed(&[])
    }
}

impl<G, S> Bot<G, S>
where
    G: GerritCommandRunner,
    S: SparkClient,
{
    pub fn run(
        self,
        // TODO: gerrit event stream probably shouldn't produce errors
        gerrit_events: impl Stream<Item = gerrit::Event, Error = ()> + Send,
        spark_messages: impl Stream<Item = spark::Message, Error = ()> + Send,
    ) -> impl Future<Item = (), Error = ()> {
        let _ = &self.gerrit_command_runner;
        let spark_client = self.spark_client.clone();
        let gerrit_actions = gerrit_events.filter_map(gerrit_event_to_action);
        let spark_actions = spark_messages.map(spark_message_to_action);
        let bot_for_action = std::sync::Arc::new(std::sync::Mutex::new(self));
        let bot_for_task = bot_for_action.clone();

        gerrit_actions
            .select(spark_actions)
            .filter_map(move |action| bot_for_action.lock().unwrap().update(action))
            .filter_map(move |task| bot_for_task.lock().unwrap().handle_task(task))
            .for_each(move |response| {
                debug!("Replying with: {}", response.message);
                spark_client
                    .reply(&response.person_id, &response.message)
                    .map_err(|e| error!("failed to send spark message: {}", e))
            })
    }

    /// Action controller
    /// Return an optional message to send to the user
    fn update(&mut self, action: Action) -> Option<Task> {
        match action {
        Action::Enable(person_id, email) => {
            self.state.enable(&person_id, &email, true);
            let task = Task::ReplyAndSave(Response::new(person_id, "Got it! Happy reviewing!"));
            Some(task)
        }
        Action::Disable(person_id, email) => {
            self.state.enable(&person_id, &email, false);
            let task = Task::ReplyAndSave(Response::new(person_id, "Got it! I will stay silent."));
            Some(task)
        }
        Action::UpdateApprovals(event) => {
            self.get_approvals_msg(&event).map(|(user, message, _is_human)|
                if definitely_has_inline_comments(&event) {
                    Task::Reply(Response::new(
                        user.spark_person_id.clone(),
                        format_msg_with_comments(message, event.change, event.patchset)))
                } else {
                    Task::Reply(Response::new(user.spark_person_id.clone(), message))
                })
        }
        Action::Help(person_id) => Some(Task::Reply(Response::new(person_id, HELP_MSG))),
            Action::Version(person_id) => Some(Task::Reply(Response::new(person_id, VERSION_MSG))),
        Action::Unknown(person_id) => Some(Task::Reply(Response::new(person_id, GREETINGS_MSG))),
        Action::Status(person_id) => {
            let status = self.status_for(&person_id);
            Some(Task::Reply(Response::new(person_id, status)))
        }
        Action::FilterStatus(person_id) => {
            let resp: String = match self.state.get_filter(&person_id) {
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
            Some(match self.state.add_filter(&person_id, filter) {
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
            Some(match self.state.enable_filter(&person_id, true) {
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
            Some(match self.state.enable_filter(&person_id, false) {
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
            self.get_reviewer_added_msg(&event).map(|(user, message)| {
                Task::Reply(Response::new(user.spark_person_id.clone(), message))
            })
        }
    }
    }

    fn handle_task(&mut self, task: Task) -> Option<Response> {
        debug!("New task {:#?}", task);
        let response = match task {
            Task::Reply(response) => Some(response),
            Task::ReplyAndSave(response) => {
                self.save("state.json")
                    .map_err(|err| {
                        error!("Could not save state: {:?}", err);
                    })
                    .ok();
                Some(response)
            }
        };
        return response;
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

    fn format_msg(
        script_source: &str,
        event: &gerrit::Event,
        approval: &gerrit::Approval,
        is_human: bool,
    ) -> Result<String, String> {
        fn create_lua_event<'lua>(
            context: rlua::Context<'lua>,
            event: &gerrit::Event,
            approval: &gerrit::Approval,
            is_human: bool,
        ) -> Result<rlua::Table<'lua>, rlua::Error> {
            let lua_event = context.create_table()?;
            lua_event.set(
                "approver",
                event
                    .author
                    .as_ref()
                    .map(|user| user.username.as_ref())
                    .unwrap_or("<unknown user>")
                    .to_string(),
            )?;
            lua_event.set("comment", event.comment.clone())?;
            lua_event.set("value", approval.value.parse().unwrap_or(0))?;
            lua_event.set("type", approval.approval_type.clone())?;
            lua_event.set("url", event.change.url.clone())?;
            lua_event.set("subject", event.change.subject.clone())?;
            lua_event.set("project", event.change.project.clone())?;
            lua_event.set("is_human", is_human)?;
            Ok(lua_event)
        }

        let lua = Lua::new();
        lua.context(|context| -> Result<String, String> {
            let globals = context.globals();
            context
                .load(script_source)
                .exec()
                .map_err(|err| format!("syntax error: {}", err))?;
            let f: LuaFunction = globals
                .get("main")
                .map_err(|_| "main function missing".to_string())?;
            let lua_event = create_lua_event(context, event, approval, is_human)
                .map_err(|err| format!("failed to create lua event table: {}", err))?;

            f.call::<_, String>(lua_event)
                .map_err(|err| format!("lua formatting function failed: {}", err))
        })
    }

    fn get_approvals_msg(&mut self, event: &gerrit::Event) -> Option<(&User, String, bool)> {
        debug!("Incoming approvals: {:#?}", event);

        if event.approvals.is_none() {
            return None;
        }

        let approvals = event.approvals.as_ref()?;
        let change = &event.change;
        let approver = &event.author.as_ref()?.username;
        if approver == &change.owner.username {
            // No need to notify about user's own approvals.
            return None;
        }
        let owner_email = change
            .owner
            .email
            .as_ref()
            .cloned()
            .map(spark::Email::new)?;

        // try to find the use and check it is enabled
        let user_pos = *self.state.email_index.get(&owner_email)?;
        if !self.state.users[user_pos].enabled {
            return None;
        }

        let is_human = !approver.to_lowercase().contains("bot");

        let msgs: Vec<String> = approvals
            .iter()
            .filter_map(|approval| {
                // filter if there was no previous value, or value did not change, or it is 0
                let filtered = !approval
                    .old_value
                    .as_ref()
                    .map(|old_value| old_value != &approval.value && approval.value != "0")
                    .unwrap_or(false);
                debug!("Filtered approval: {:?}", filtered);
                if filtered {
                    return None;
                }

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

                let msg = match Self::format_msg(&self.format_script, event, approval, is_human) {
                    Ok(msg) => msg,
                    Err(err) => {
                        error!("message formatting failed: {}", err);
                        return None;
                    }
                };

                // if user has configured and enabled a filter try to apply it
                if self.state.is_filtered(user_pos, &msg) {
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
            // We got some approvals
            Some((&self.state.users[user_pos], msgs.join("\n\n"), is_human)) // two newlines since it is markdown
        } else if is_human && definitely_has_inline_comments(&event) && event.comment.is_some() {
            // We did not get any approvals, but we got inline comments from a human.
            Some((
                &self.state.users[user_pos],
                event.comment.clone().unwrap(),
                is_human,
            ))
        } else {
            None
        }
    }

    fn get_reviewer_added_msg(&mut self, event: &gerrit::Event) -> Option<(&User, String)> {
        let reviewer = event.reviewer.as_ref().cloned()?;
        let reviewer_email = reviewer.email.as_ref().cloned().map(spark::Email::new)?;
        let user_pos = *self.state.email_index.get(&reviewer_email)?;
        if !self.state.users[user_pos].enabled {
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
            &self.state.users[user_pos],
            format!(
                "[{}]({}) ({}) 👓 Added as reviewer",
                event.change.subject, event.change.url, event.change.owner.username
            ),
        ))
    }

    pub fn save<P>(&self, filename: P) -> Result<(), BotError>
    where
        P: AsRef<Path>,
    {
        let f = File::create(filename)?;
        serde_json::to_writer(f, &self.state)?;
        Ok(())
    }

    pub fn status_for(&self, person_id: &spark::PersonIdRef) -> String {
        let user = self.state.find_user(person_id);
        let enabled = user.map_or(false, |u| u.enabled);
        format!(
            "Notifications for you are **{}**. I am notifying another {} user(s).",
            if enabled { "enabled" } else { "disabled" },
            if self.state.num_users() == 0 {
                0
            } else {
                self.state.num_users() - if enabled { 1 } else { 0 }
            }
        )
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
    Version(spark::PersonId),
    FilterStatus(spark::PersonId),
    FilterAdd(spark::PersonId, String /* filter */),
    FilterEnable(spark::PersonId),
    FilterDisable(spark::PersonId),
    ReviewerAdded(Box<gerrit::Event>),
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
            person_id,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub enum Task {
    Reply(Response),
    ReplyAndSave(Response),
}

const GREETINGS_MSG: &str =
r#"Hi. I am GerritBot. I can watch Gerrit reviews for you, and notify you about new +1/-1's.

To enable notifications, just type in **enable**. A small note: your email in Spark and in Gerrit has to be the same. Otherwise, I can't match your accounts.

For more information, type in **help**.
"#;

const HELP_MSG: &str = r#"Commands:

`enable` -- I will start notifying you.

`disable` -- I will stop notifying you.

`filter <regex>` -- Filter all messages by applying the specified regex pattern. If the pattern matches, the message is filtered. The pattern is applied to the full text I send to you. Be aware, to send this command **not** in markdown mode, otherwise, Spark would eat some special characters in the pattern. For regex specification, cf. https://docs.rs/regex/0.2.10/regex/#syntax.

`filter enable` -- Enable the filtering of messages with the configured filter.

`filter disable` -- Disable the filtering of messages with the configured filter.

`status` -- Show if I am notifying you, and a little bit more information. 😉

`help` -- This message

This project is open source, feel free to help us at: https://github.com/boxdot/gerritbot-rs
"#;

const VERSION_MSG: &str = concat!(
    env!("CARGO_PKG_NAME"),
    " ",
    env!("CARGO_PKG_VERSION"),
    " (commit id ",
    env!("VERGEN_SHA"),
    ")"
);

/// Guess if the change might have comments by looking for a specially formatted
/// comment.
fn maybe_has_inline_comments(event: &gerrit::Event) -> bool {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"\(\d+\scomments?\)").unwrap();
    }
    event
        .comment
        .as_ref()
        .map(|s| RE.is_match(s))
        .unwrap_or(false)
}

fn definitely_has_inline_comments(event: &gerrit::Event) -> bool {
    event
        .patchset
        .comments
        .as_ref()
        .map(|c| !c.is_empty())
        .unwrap_or(false)
}

fn format_comments(change: gerrit::Change, patchset: gerrit::PatchSet) -> Option<String> {
    let change_number = change.number;
    let host = change.url.split('/').nth(2).unwrap();
    let patch_set_number = patchset.number;

    patchset.comments.map(|mut comments| {
        comments.sort_by(|a, b| a.file.cmp(&b.file));
        comments
            .into_iter()
            .group_by(|c| c.file.clone())
            .into_iter()
            .map(|(file, comments)| -> String {
                let line_comments = comments
                    .map(|comment| {
                        let mut lines = comment.message.split('\n');
                        let url = format!(
                            "https://{}/#/c/{}/{}/{}@{}",
                            host, change_number, patch_set_number, comment.file, comment.line
                        );
                        let first_line = lines.next().unwrap_or("");
                        let first_line = format!(
                            "> [Line {}]({}) by {}: {}",
                            comment.line, url, comment.reviewer, first_line
                        );
                        let tail = lines
                            .map(|l| format!("> {}", l))
                            .intersperse("\n".into())
                            .collect::<Vec<_>>()
                            .concat();
                        format!("{}\n{}", first_line, tail)
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

fn format_msg_with_comments(
    message: String,
    change: gerrit::Change,
    patchset: gerrit::PatchSet,
) -> String {
    if let Some(additional_message) = format_comments(change, patchset) {
        format!("{}\n\n{}", message, additional_message)
    } else {
        message
    }
}

#[cfg(test)]
mod test {
    use std::thread;
    use std::time::Duration;

    use futures::future;
    use spectral::prelude::*;
    use speculate::speculate;

    use spark::{EmailRef, PersonId, PersonIdRef};

    use super::*;

    struct TestGerritCommandRunner;
    impl GerritCommandRunner for TestGerritCommandRunner {}

    #[derive(Clone)]
    struct TestSparkClient;

    type TestBot = Bot<TestGerritCommandRunner, TestSparkClient>;

    impl SparkClient for TestSparkClient {
        type ReplyFuture = future::FutureResult<(), spark::Error>;
        fn reply(&self, _person_id: &PersonId, _msg: &str) -> Self::ReplyFuture {
            future::ok(())
        }
    }

    impl TestBot {
        fn add_user(&mut self, person_id: &str, email: &str) {
            self.state
                .add_user(PersonIdRef::new(person_id), EmailRef::new(email));
        }

        fn enable(&mut self, person_id: &str, email: &str, enabled: bool) {
            self.state
                .enable(PersonIdRef::new(person_id), EmailRef::new(email), enabled);
        }
    }

    fn new_bot() -> TestBot {
        Builder::new(State::new()).build(TestGerritCommandRunner, TestSparkClient)
    }

    fn new_bot_with_msg_cache(capacity: usize, expiration: Duration) -> TestBot {
        Builder::new(State::new())
            .with_msg_cache(capacity, expiration)
            .build(TestGerritCommandRunner, TestSparkClient)
    }

    trait UserAssertions {
        fn has_person_id(&mut self, expected: &str);
        fn has_email(&mut self, expected: &str);
        fn is_enabled(&mut self);
        fn is_not_enabled(&mut self);
    }

    impl<'s> UserAssertions for spectral::Spec<'s, &User> {
        fn has_person_id(&mut self, expected: &str) {
            let actual = &self.subject.spark_person_id;
            let expected = PersonIdRef::new(expected);
            if actual != expected {
                spectral::AssertionFailure::from_spec(self)
                    .with_expected(format!("user with name <{}>", expected))
                    .with_actual(format!("<{}>", actual))
                    .fail();
            }
        }

        fn has_email(&mut self, expected: &str) {
            let actual = &self.subject.email;
            let expected = EmailRef::new(expected);
            if actual != expected {
                spectral::AssertionFailure::from_spec(self)
                    .with_expected(format!("user with email <{}>", expected))
                    .with_actual(format!("<{}>", actual))
                    .fail();
            }
        }

        fn is_enabled(&mut self) {
            if !self.subject.enabled {
                spectral::AssertionFailure::from_spec(self)
                    .with_expected("user is enabled".to_string())
                    .with_actual("it is not".to_string())
                    .fail();
            }
        }

        fn is_not_enabled(&mut self) {
            if self.subject.enabled {
                spectral::AssertionFailure::from_spec(self)
                    .with_expected("user is not enabled".to_string())
                    .with_actual("it is".to_string())
                    .fail();
            }
        }
    }

    trait HasItemMatchingAssertion<'s, T: 's> {
        fn has_item_matching<P>(&mut self, predicate: P)
        where
            P: FnMut(&'s T) -> bool;
    }

    impl<'s, T: 's, I> HasItemMatchingAssertion<'s, T> for spectral::Spec<'s, I>
    where
        T: std::fmt::Debug,
        &'s I: IntoIterator<Item = &'s T>,
    {
        fn has_item_matching<P>(&mut self, predicate: P)
        where
            P: FnMut(&'s T) -> bool,
        {
            let subject = self.subject;

            if !subject.into_iter().any(predicate) {
                spectral::AssertionFailure::from_spec(self)
                    .with_expected(
                        "iterator to contain an item matching the given predicate".to_string(),
                    )
                    .with_actual("it did not".to_string())
                    .fail();
            }
        }
    }

    speculate! {
        before {
            let bot = new_bot();
        }

        describe "when a user is added" {
            before {
                let mut bot = bot;
                bot.add_user("some_person_id", "some@example.com");
            }

            before {
                assert_that!(bot.state.users).has_length(1);
            }

            it "has the expected attributes" {
                let user = &bot.state.users[0];
                assert_that!(user).has_person_id("some_person_id");
                assert_that!(user).has_email("some@example.com");
                assert_that!(user).is_enabled();
            }

            test "enabled status response" {
                let resp = bot.status_for(PersonIdRef::new("some_person_id"));
                assert_that!(resp).contains("enabled");
            }

            test "disabled status response" {
                bot.state.users[0].enabled = false;
                let resp = bot.status_for(PersonIdRef::new("some_person_id"));
                assert_that!(resp).contains("disabled");
            }

            test "existing user can be enabled" {
                bot.enable("some_person_id", "some@example.com", true);
                assert_that!(bot.state.users)
                    .has_item_matching(
                        |u| u.spark_person_id == PersonIdRef::new("some_person_id")
                            && u.email == EmailRef::new("some@example.com")
                            && u.enabled);
                assert_that!(bot.state.users).has_length(1);
            }

            test "existing can be disabled" {
                bot.enable("some_person_id", "some@example.com", false);
                assert_that!(bot.state.users)
                    .has_item_matching(
                        |u| u.spark_person_id == PersonIdRef::new("some_person_id")
                            && u.email == EmailRef::new("some@example.com")
                            && !u.enabled);
                assert_that!(bot.state.users).has_length(1);
            }
        }

        test "non-existing user is automatically added when enabled" {
            assert_that!(bot.state.users).has_length(0);
            let mut bot = bot;
            bot.enable("some_person_id", "some@example.com", true);
            assert_that!(bot.state.users)
                .has_item_matching(
                    |u| u.spark_person_id == PersonIdRef::new("some_person_id")
                        && u.email == EmailRef::new("some@example.com")
                        && u.enabled);
            assert_that!(bot.state.users).has_length(1);
        }

        test "non-existing user is automatically added when disabled" {
            assert_that!(bot.state.users).has_length(0);
            let mut bot = bot;
            bot.enable("some_person_id", "some@example.com", false);
            assert_that!(bot.state.users)
                .has_item_matching(
                    |u| u.spark_person_id == PersonIdRef::new("some_person_id")
                        && u.email == EmailRef::new("some@example.com")
                        && !u.enabled);
            assert_that!(bot.state.users).has_length(1);
        }

        test "unknown user gets disabled status response" {
            let resp = bot.status_for(PersonIdRef::new("some_non_existent_id"));
            assert!(resp.contains("disabled"));
        }
    }

    const EVENT_JSON : &'static str = r#"
{"author":{"name":"Approver","username":"approver"},"approvals":[{"type":"Code-Review","description":"Code-Review","value":"2","oldValue":"-1"}],"comment":"Patch Set 1: Code-Review+2\n\nJust a buggy script. FAILURE\n\nAnd more problems. FAILURE","patchSet":{"number":1,"revision":"49a65998c02eda928559f2d0b586c20bc8e37b10","parents":["fb1909b4eda306985d2bbce769310e5a50a98cf5"],"ref":"refs/changes/42/42/1","uploader":{"name":"Author","email":"author@example.com","username":"Author"},"createdOn":1494165142,"author":{"name":"Author","email":"author@example.com","username":"Author"},"isDraft":false,"kind":"REWORK","sizeInsertions":0,"sizeDeletions":0},"change":{"project":"demo-project","branch":"master","id":"Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14","number":49,"subject":"Some review.","owner":{"name":"Author","email":"author@example.com","username":"author"},"url":"http://localhost/42","commitMessage":"Some review.\n\nChange-Id: Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14\n","status":"NEW"},"project":"demo-project","refName":"refs/heads/master","changeKey":{"id":"Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14"},"type":"comment-added","eventCreatedOn":1499190282}"#;

    const CHANGE_JSON_WITH_COMMENTS : &'static str = r#"
{"project":"gerritbot-rs","branch":"master","id":"If70442f674c595a59f3e44280570e760ba3584c4","number":1,"subject":"Bump version to 0.6.0","owner":{"name":"Administrator","email":"admin@example.com","username":"admin"},"url":"http://localhost:8080/1","commitMessage":"Bump version to 0.6.0\n\nChange-Id: If70442f674c595a59f3e44280570e760ba3584c4\n","createdOn":1524584729,"lastUpdated":1524584975,"open":true,"status":"NEW","comments":[{"timestamp":1524584729,"reviewer":{"name":"Administrator","email":"admin@example.com","username":"admin"},"message":"Uploaded patch set 1."},{"timestamp":1524584975,"reviewer":{"name":"jdoe","email":"john.doe@localhost","username":"jdoe"},"message":"Patch Set 1:\n\n(1 comment)"}]}"#;
    const PATCHSET_JSON_WITH_COMMENTS : &'static str = r#"{"number":1,"revision":"3f58af760fc1e39fcc4a85b8ab6a6be032cf2ae2","parents":["578bc1e684098d2ac597e030442c3472f15ac3ad"],"ref":"refs/changes/01/1/1","uploader":{"name":"Administrator","email":"admin@example.com","username":"admin"},"createdOn":1524584729,"author":{"name":"jdoe","email":"jdoe@example.com","username":""},"isDraft":false,"kind":"REWORK","comments":[{"file":"/COMMIT_MSG","line":1,"reviewer":{"name":"jdoe","email":"john.doe@localhost","username":"jdoe"},"message":"This is a multiline\ncomment\non some change."}],"sizeInsertions":2,"sizeDeletions":-2}"#;

    fn get_event() -> gerrit::Event {
        let event: Result<gerrit::Event, _> = serde_json::from_str(EVENT_JSON);
        assert!(event.is_ok());
        event.unwrap()
    }

    fn get_change_with_comments() -> (gerrit::Change, gerrit::PatchSet) {
        let change: Result<gerrit::Change, _> = serde_json::from_str(CHANGE_JSON_WITH_COMMENTS);
        assert!(change.is_ok());
        let patchset: Result<gerrit::PatchSet, _> =
            serde_json::from_str(PATCHSET_JSON_WITH_COMMENTS);
        assert!(patchset.is_ok());
        (change.unwrap(), patchset.unwrap())
    }

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
    fn get_approvals_msg_for_empty_bot() {
        // bot does not have the user => no message
        let mut bot = new_bot();
        let res = bot.get_approvals_msg(&get_event());
        assert!(res.is_none());
    }

    #[test]
    fn get_approvals_msg_for_same_author_and_approver() {
        // the approval is from the author => no message
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("approver_spark_id"),
            EmailRef::new("approver@example.com"),
        );
        let res = bot.get_approvals_msg(&get_event());
        assert!(res.is_none());
    }

    #[test]
    fn get_approvals_msg_for_user_with_disabled_notifications() {
        // the approval is for the user with disabled notifications
        // => no message
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("author_spark_id"),
            EmailRef::new("author@example.com"),
        );
        bot.state.users[0].enabled = false;
        let res = bot.get_approvals_msg(&get_event());
        assert!(res.is_none());
    }

    #[test]
    fn get_approvals_msg_for_user_with_enabled_notifications() {
        // the approval is for the user with enabled notifications
        // => message
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("author_spark_id"),
            EmailRef::new("author@example.com"),
        );
        let res = bot.get_approvals_msg(&get_event());
        assert!(res.is_some());
        let (user, msg, is_human) = res.unwrap();
        assert_eq!(user.spark_person_id, PersonIdRef::new("author_spark_id"));
        assert_eq!(user.email, EmailRef::new("author@example.com"));
        assert!(msg.contains("Some review."));
        assert!(is_human);
    }

    #[test]
    fn get_approvals_msg_for_user_with_enabled_notifications_and_filter() {
        // the approval is for the user with enabled notifications
        // => message
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("author_spark_id"),
            EmailRef::new("author@example.com"),
        );

        {
            let res = bot
                .state
                .add_filter(PersonIdRef::new("author_spark_id"), ".*Code-Review.*");
            assert!(res.is_ok());
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_none());
        }
        {
            let res = bot
                .state
                .enable_filter(PersonIdRef::new("author_spark_id"), false);
            assert!(res.is_ok());
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, PersonIdRef::new("author_spark_id"));
            assert_eq!(user.email, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
        {
            let res = bot
                .state
                .enable_filter(PersonIdRef::new("author_spark_id"), true);
            assert!(res.is_ok());
            let res = bot.state.add_filter(
                PersonIdRef::new("author_spark_id"),
                "some_non_matching_filter",
            );
            assert!(res.is_ok());
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, PersonIdRef::new("author_spark_id"));
            assert_eq!(user.email, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
    }

    #[test]
    fn get_approvals_msg_for_quickly_repeated_event() {
        // same approval for the user with enabled notifications 2 times in less than 1 sec
        // => first time get message, second time nothing
        let mut bot = new_bot_with_msg_cache(10, Duration::from_secs(1));
        bot.state.add_user(
            PersonIdRef::new("author_spark_id"),
            EmailRef::new("author@example.com"),
        );
        {
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, PersonIdRef::new("author_spark_id"));
            assert_eq!(user.email, EmailRef::new("author@example.com"));
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
        let mut bot = new_bot_with_msg_cache(10, Duration::from_millis(50));
        bot.state.add_user(
            PersonIdRef::new("author_spark_id"),
            EmailRef::new("author@example.com"),
        );
        {
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, PersonIdRef::new("author_spark_id"));
            assert_eq!(user.email, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
        thread::sleep(Duration::from_millis(200));
        {
            let res = bot.get_approvals_msg(&get_event());
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, PersonIdRef::new("author_spark_id"));
            assert_eq!(user.email, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
    }

    #[test]
    fn get_approvals_msg_for_bot_with_low_msgs_capacity() {
        // same approval for the user with enabled notifications 2 times in more less 100 msec
        // but there is also another approval and bot's msg capacity is 1
        // => get message 3 times
        let mut bot = new_bot_with_msg_cache(1, Duration::from_secs(1));
        bot.state.add_user(
            PersonIdRef::new("author_spark_id"),
            EmailRef::new("author@example.com"),
        );
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
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("author_spark_id"),
            EmailRef::new("author@example.com"),
        );
        let event = get_event();
        let res = TestBot::format_msg(
            get_default_format_script(),
            &event,
            &event.approvals.as_ref().unwrap()[0],
            true,
        );
        assert_eq!(
            res,
            Ok("[Some review.](http://localhost/42) (demo-project) 👍 +2 (Code-Review) from approver\n\n> Just a buggy script. FAILURE<br>\n> And more problems. FAILURE".to_string())
        );
    }

    #[test]
    fn format_msg_filters_specific_messages() {
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("author_spark_id"),
            EmailRef::new("author@example.com"),
        );
        let mut event = get_event();
        event.approvals.as_mut().unwrap()[0].approval_type = String::from("Some new type");
        let res = TestBot::format_msg(
            get_default_format_script(),
            &event,
            &event.approvals.as_ref().unwrap()[0],
            true,
        );
        assert_eq!(res.map(|s| s.is_empty()), Ok(true));
    }

    #[test]
    fn add_invalid_filter_for_existing_user() {
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );
        let res = bot
            .state
            .add_filter(PersonIdRef::new("some_person_id"), ".some_weard_regex/[");
        assert_eq!(res, Err(AddFilterResult::InvalidFilter));
        assert!(bot
            .state
            .users
            .iter()
            .position(|u| u.spark_person_id == PersonIdRef::new("some_person_id")
                && u.email == EmailRef::new("some@example.com")
                && u.filter == None)
            .is_some());

        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
    }

    #[test]
    fn add_valid_filter_for_existing_user() {
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );

        let res = bot
            .state
            .add_filter(PersonIdRef::new("some_person_id"), ".*some_word.*");
        assert!(res.is_ok());
        assert!(bot
            .state
            .users
            .iter()
            .position(|u| u.spark_person_id == PersonIdRef::new("some_person_id")
                && u.email == EmailRef::new("some@example.com")
                && u.filter == Some(Filter::new(".*some_word.*")))
            .is_some());

        {
            let filter = bot.state.get_filter(PersonIdRef::new("some_person_id"));
            assert_eq!(filter, Ok(Some(&Filter::new(".*some_word.*"))));
        }
        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Ok(String::from(".*some_word.*")));
        assert!(bot
            .state
            .users
            .iter()
            .position(|u| u.spark_person_id == PersonIdRef::new("some_person_id")
                && u.email == EmailRef::new("some@example.com")
                && u.filter.as_ref().map(|f| f.enabled) == Some(false))
            .is_some());
        {
            let filter = bot
                .state
                .get_filter(PersonIdRef::new("some_person_id"))
                .unwrap()
                .unwrap();
            assert_eq!(filter.regex, ".*some_word.*");
            assert_eq!(filter.enabled, false);
        }
        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Ok(String::from(".*some_word.*")));
        assert!(bot
            .state
            .users
            .iter()
            .position(|u| u.spark_person_id == PersonIdRef::new("some_person_id")
                && u.email == EmailRef::new("some@example.com")
                && u.filter.as_ref().map(|f| f.enabled) == Some(true))
            .is_some());
        {
            let filter = bot.state.get_filter(PersonIdRef::new("some_person_id"));
            assert_eq!(filter, Ok(Some(&Filter::new(".*some_word.*"))));
        }
    }

    #[test]
    fn add_valid_filter_for_non_existing_user() {
        let mut bot = new_bot();
        let res = bot
            .state
            .add_filter(PersonIdRef::new("some_person_id"), ".*some_word.*");
        assert_eq!(res, Err(AddFilterResult::UserNotFound));
        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Err(AddFilterResult::UserNotFound));
        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Err(AddFilterResult::UserNotFound));
    }

    #[test]
    fn add_valid_filter_for_disabled_user() {
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );
        bot.state.users[0].enabled = false;

        let res = bot
            .state
            .add_filter(PersonIdRef::new("some_person_id"), ".*some_word.*");
        assert_eq!(res, Err(AddFilterResult::UserDisabled));
        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Err(AddFilterResult::UserDisabled));
        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Err(AddFilterResult::UserDisabled));
    }

    #[test]
    fn enable_non_configured_filter_for_existing_user() {
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );

        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Err(AddFilterResult::FilterNotConfigured));
    }

    #[test]
    fn enable_invalid_filter_for_existing_user() {
        let mut bot = new_bot();
        bot.state.add_user(
            PersonIdRef::new("some_person_id"),
            EmailRef::new("some@example.com"),
        );
        bot.state.users[0].filter = Some(Filter::new("invlide_filter_set_from_outside["));

        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), true);
        assert_eq!(res, Err(AddFilterResult::InvalidFilter));
        let res = bot
            .state
            .enable_filter(PersonIdRef::new("some_person_id"), false);
        assert_eq!(res, Err(AddFilterResult::InvalidFilter));
    }

    #[test]
    fn test_maybe_has_inline_comments() {
        let mut event = get_event();
        event.comment = Some("PatchSet 666: (2 comments)".into());
        assert!(maybe_has_inline_comments(&event));

        event.comment = Some("Nope, colleague comment!".into());
        assert!(!maybe_has_inline_comments(&event));
    }

    #[test]
    fn test_format_comments() {
        let (change, mut patchset) = get_change_with_comments();
        patchset.comments = None;
        assert_eq!(format_comments(change, patchset), None);

        let (change, patchset) = get_change_with_comments();
        assert_eq!(format_comments(change, patchset),
            Some("`/COMMIT_MSG`\n\n> [Line 1](https://localhost:8080/#/c/1/1//COMMIT_MSG@1) by jdoe: This is a multiline\n> comment\n> on some change.".into()));
    }
}
