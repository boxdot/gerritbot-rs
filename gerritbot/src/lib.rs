use std::borrow::Cow;
use std::convert;
use std::fs::File;
use std::io;
use std::path::Path;
use std::time::Duration;

use futures::{future::Future, stream::Stream};
use lazy_static::lazy_static;
use log::{debug, error};
use regex::Regex;

use gerritbot_gerrit as gerrit;
use gerritbot_spark as spark;

pub mod args;
mod format;
mod rate_limit;
mod state;

use format::Formatter;
pub use format::DEFAULT_FORMAT_SCRIPT;
use rate_limit::RateLimiter;
pub use state::State;
use state::{AddFilterResult, User};

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
    rate_limiter: RateLimiter,
    formatter: format::Formatter,
    gerrit_command_runner: G,
    spark_client: S,
}

#[derive(Debug, Clone, Copy)]
struct MsgCacheParameters {
    capacity: usize,
    expiration: Duration,
}

#[derive(Default)]
pub struct Builder {
    state: State,
    rate_limiter: RateLimiter,
    formatter: Formatter,
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

impl Builder {
    pub fn new(state: State) -> Self {
        Self {
            state,
            ..Default::default()
        }
    }

    pub fn with_msg_cache(self, capacity: usize, expiration: Duration) -> Self {
        Self {
            rate_limiter: RateLimiter::with_expiry_duration_and_capacity(expiration, capacity),
            ..self
        }
    }

    pub fn with_format_script(self, script_source: &str) -> Result<Self, String> {
        Ok(Self {
            formatter: Formatter::new(script_source)?,
            ..self
        })
    }

    pub fn build<G, S>(self, gerrit_command_runner: G, spark_client: S) -> Bot<G, S> {
        let Self {
            formatter,
            rate_limiter,
            state,
        } = self;

        Bot {
            gerrit_command_runner,
            spark_client,
            rate_limiter,
            formatter,
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
    match event {
        gerrit::Event::CommentAdded(event) => Some(Action::UpdateApprovals(Box::new(event))),
        gerrit::Event::ReviewerAdded(event) => Some(Action::ReviewerAdded(Box::new(event))),
    }
}

pub fn request_extended_gerrit_info(event: &gerrit::Event) -> Cow<'static, [gerrit::ExtendedInfo]> {
    let mut extended_info = Vec::new();

    match event {
        gerrit::Event::CommentAdded(event) => {
            let owner_name = event.change.owner.username.as_ref();
            let approver_name = event.author.username.as_ref();
            let is_human = approver_name
                .map(|name| !name.to_lowercase().contains("bot"))
                .unwrap_or(false);

            if owner_name != approver_name && is_human && maybe_has_inline_comments(event) {
                extended_info.push(gerrit::ExtendedInfo::InlineComments);
            }

            // Could be smarter here by checking for old_value and if the value
            // is positive.
            extended_info.push(gerrit::ExtendedInfo::SubmitRecords);
        }
        _ => (),
    }

    Cow::Owned(extended_info)
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
            self.get_approvals_msg(event).map(|(user, message, _is_human)|
                    Task::Reply(Response::new(user.spark_person_id.clone(), message)))
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

    fn get_approvals_msg(
        &mut self,
        event: Box<gerrit::CommentAddedEvent>,
    ) -> Option<(&User, String, bool)> {
        debug!("Incoming approvals: {:#?}", event);

        let approvals = &event.approvals;
        let change = &event.change;
        let approver = event.author.username.as_ref()?;
        if Some(approver) == change.owner.username.as_ref() {
            // No need to notify about user's own approvals.
            return None;
        }
        let owner_email = spark::Email::new(change.owner.email.clone());

        // try to find the use and check it is enabled
        let user_pos = *self.state.email_index.get(&owner_email)?;
        if !self.state.users[user_pos].enabled {
            return None;
        }

        let is_human = !approver.to_lowercase().contains("bot");

        // filter all messages that were already sent to the user recently
        if !approvals.is_empty() && self.rate_limiter.limit(user_pos, &*event) {
            debug!("Filtered approval due to cache hit.");
            return None;
        }
        let user = &self.state.users[user_pos];

        self.formatter
            .format_comment_added(&event, is_human)
            .unwrap_or_else(|e| {
                error!("message formatting failed: {}", e);
                None
            })
            .filter(|msg| {
                // if user has configured and enabled a filter try to apply it
                !self.state.is_filtered(user_pos, &msg)
            })
            .map(|m| (user, m, is_human))
    }

    fn get_reviewer_added_msg(
        &mut self,
        event: &gerrit::ReviewerAddedEvent,
    ) -> Option<(&User, String)> {
        let reviewer = event.reviewer.clone();
        let reviewer_email = spark::Email::new(reviewer.email.clone());
        let user_pos = *self.state.email_index.get(&reviewer_email)?;
        if !self.state.users[user_pos].enabled {
            return None;
        }

        // filter all messages that were already sent to the user recently
        if self.rate_limiter.limit(user_pos, event) {
            debug!("Filtered reviewer-added due to cache hit.");
            return None;
        }

        let message = self.formatter.format_reviewer_added(event).ok()?;

        Some((&self.state.users[user_pos], message))
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
        let enabled_user_count =
            self.state.users.iter().filter(|u| u.enabled).count() - if enabled { 1 } else { 0 };
        format!(
            "Notifications for you are **{}**. I am notifying {}.",
            if enabled { "enabled" } else { "disabled" },
            match (enabled, enabled_user_count) {
                (false, 0) => format!("no users"),
                (true, 0) => format!("no other users"),
                (false, 1) => format!("one user"),
                (true, 1) => format!("another user"),
                (false, _) => format!("{} users", enabled_user_count),
                (true, _) => format!("another {} users", enabled_user_count),
            }
        )
    }
}

#[derive(Debug)]
pub enum Action {
    Enable(spark::PersonId, spark::Email),
    Disable(spark::PersonId, spark::Email),
    UpdateApprovals(Box<gerrit::CommentAddedEvent>),
    Help(spark::PersonId),
    Unknown(spark::PersonId),
    Status(spark::PersonId),
    Version(spark::PersonId),
    FilterStatus(spark::PersonId),
    FilterAdd(spark::PersonId, String /* filter */),
    FilterEnable(spark::PersonId),
    FilterDisable(spark::PersonId),
    ReviewerAdded(Box<gerrit::ReviewerAddedEvent>),
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
fn maybe_has_inline_comments(event: &gerrit::CommentAddedEvent) -> bool {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"\(\d+\scomments?\)").unwrap();
    }
    RE.is_match(&event.comment)
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
{"author":{"name":"Approver","username":"approver","email":"approver@approvers.com"},"approvals":[{"type":"Code-Review","description":"Code-Review","value":"2","oldValue":"-1"}],"comment":"Patch Set 1: Code-Review+2\n\nJust a buggy script. FAILURE\n\nAnd more problems. FAILURE","patchSet":{"number":1,"revision":"49a65998c02eda928559f2d0b586c20bc8e37b10","parents":["fb1909b4eda306985d2bbce769310e5a50a98cf5"],"ref":"refs/changes/42/42/1","uploader":{"name":"Author","email":"author@example.com","username":"Author"},"createdOn":1494165142,"author":{"name":"Author","email":"author@example.com","username":"Author"},"isDraft":false,"kind":"REWORK","sizeInsertions":0,"sizeDeletions":0},"change":{"project":"demo-project","branch":"master","id":"Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14","number":49,"subject":"Some review.","owner":{"name":"Author","email":"author@example.com","username":"author"},"url":"http://localhost/42","commitMessage":"Some review.\n\nChange-Id: Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14\n","status":"NEW"},"project":"demo-project","refName":"refs/heads/master","changeKey":{"id":"Ic160fa37fca005fec17a2434aadf0d9dcfbb7b14"},"type":"comment-added","eventCreatedOn":1499190282}"#;

    fn get_event() -> gerrit::CommentAddedEvent {
        let event: Result<gerrit::Event, _> = serde_json::from_str(EVENT_JSON);
        match event.expect("failed to decode event") {
            gerrit::Event::CommentAdded(event) => event,
            event => panic!("wrong type of event: {:?}", event),
        }
    }

    #[test]
    fn get_approvals_msg_for_empty_bot() {
        // bot does not have the user => no message
        let mut bot = new_bot();
        let res = bot.get_approvals_msg(Box::new(get_event()));
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
        let res = bot.get_approvals_msg(Box::new(get_event()));
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
        let res = bot.get_approvals_msg(Box::new(get_event()));
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
        let res = bot.get_approvals_msg(Box::new(get_event()));
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
            let res = bot.get_approvals_msg(Box::new(get_event()));
            assert!(res.is_none());
        }
        {
            let res = bot
                .state
                .enable_filter(PersonIdRef::new("author_spark_id"), false);
            assert!(res.is_ok());
            let res = bot.get_approvals_msg(Box::new(get_event()));
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
            let res = bot.get_approvals_msg(Box::new(get_event()));
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
            let res = bot.get_approvals_msg(Box::new(get_event()));
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, PersonIdRef::new("author_spark_id"));
            assert_eq!(user.email, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
        {
            let res = bot.get_approvals_msg(Box::new(get_event()));
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
            let res = bot.get_approvals_msg(Box::new(get_event()));
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.spark_person_id, PersonIdRef::new("author_spark_id"));
            assert_eq!(user.email, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
        thread::sleep(Duration::from_millis(200));
        {
            let res = bot.get_approvals_msg(Box::new(get_event()));
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
            let res = bot.get_approvals_msg(Box::new(event));
            assert!(res.is_some());
        }
        {
            let mut event = get_event();
            event.change.subject = String::from("B");
            let res = bot.get_approvals_msg(Box::new(event));
            assert!(res.is_some());
        }
        {
            let mut event = get_event();
            event.change.subject = String::from("A");
            let res = bot.get_approvals_msg(Box::new(event));
            assert!(res.is_some());
        }
    }

    #[test]
    fn test_maybe_has_inline_comments() {
        let mut event = get_event();
        event.comment = "PatchSet 666: (2 comments)".to_string();
        assert!(maybe_has_inline_comments(&event));

        event.comment = "Nope, colleague comment!".to_string();
        assert!(!maybe_has_inline_comments(&event));
    }
}
