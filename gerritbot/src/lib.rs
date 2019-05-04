use std::borrow::Cow;
use std::convert;
use std::fs::File;
use std::io;
use std::path::Path;
use std::time::Duration;

use futures::{future::Future, stream, stream::Stream};
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
use state::User;

pub trait GerritCommandRunner {}

impl GerritCommandRunner for gerrit::CommandRunner {}

pub trait SparkClient: Clone {
    type ReplyFuture: Future<Item = (), Error = spark::Error> + Send;
    fn send_message(&self, email: &spark::EmailRef, msg: &str) -> Self::ReplyFuture;
}

impl SparkClient for spark::Client {
    type ReplyFuture = Box<dyn Future<Item = (), Error = spark::Error> + Send>;
    fn send_message(&self, email: &spark::EmailRef, msg: &str) -> Self::ReplyFuture {
        Box::new(self.send_message(email, msg))
    }
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

#[derive(Default)]
pub struct Builder {
    state: State,
    rate_limiter: RateLimiter,
    formatter: Formatter,
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
    match &message.text.trim().to_lowercase()[..] {
        "enable" => Action::Enable(sender_email),
        "disable" => Action::Disable(sender_email),
        "status" => Action::Status(sender_email),
        "help" => Action::Help(sender_email),
        "version" => Action::Version(sender_email),
        "filter" => Action::FilterStatus(sender_email),
        "filter enable" => Action::FilterEnable(sender_email, true),
        "filter disable" => Action::FilterEnable(sender_email, false),
        _ => FILTER_REGEX
            .captures(&message.text.trim()[..])
            .and_then(|cap| cap.get(1))
            .map(|m| Action::FilterAdd(sender_email.clone(), m.as_str().to_string()))
            .unwrap_or_else(|| Action::Unknown(sender_email.clone())),
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

pub struct Bot<G = gerrit::CommandRunner, S = spark::Client> {
    state: State,
    rate_limiter: RateLimiter,
    formatter: format::Formatter,
    gerrit_command_runner: G,
    spark_client: S,
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
            .map(move |action| bot_for_action.lock().unwrap().update(action))
            .map(stream::iter_ok)
            .flatten()
            .filter_map(move |task| bot_for_task.lock().unwrap().handle_task(task))
            .map(move |response| {
                debug!("Replying with: {}", response.message);
                spark_client.send_message(&response.email, &response.message)
            })
            .map(|send_future| {
                // try sending a message for up to 5 seconds, then give up
                tokio::timer::Timeout::new(send_future, Duration::from_secs(5))
                    .map_err(|e| error!("failed to send spark message: {}", e))
            })
            // try sending up to 10 messages at a time
            .buffer_unordered(10)
            .for_each(|()| Ok(()))
    }

    /// Action controller
    /// Return an optional message to send to the user
    fn update(&mut self, action: Action) -> Vec<Task> {
        match action {
            Action::Enable(email) => {
                self.state.enable(&email, true);
                vec![
                    Task::Save,
                    Task::Reply(Response::new(email, "Got it! Happy reviewing!")),
                ]
            }
            Action::Disable(email) => {
                self.state.enable(&email, false);
                vec![
                    Task::Save,
                    Task::Reply(Response::new(email, "Got it! I will stay silent.")),
                ]
            }
            Action::UpdateApprovals(event) => self
                .get_approvals_msg(event)
                .map(|(user, message, _is_human)| {
                    Task::Reply(Response::new(user.email().to_owned(), message))
                })
                .into_iter()
                .collect(),
            Action::Help(email) => vec![Task::Reply(Response::new(email, HELP_MSG))],
            Action::Version(email) => vec![Task::Reply(Response::new(email, VERSION_MSG))],
            Action::Unknown(email) => vec![Task::Reply(Response::new(email, GREETINGS_MSG))],
            Action::Status(email) => {
                let status = self.status_for(&email);
                vec![Task::Reply(Response::new(email, status))]
            }
            Action::FilterStatus(email) => {
                let resp = if let Some((filter_str, filter_enabled)) = self.state.get_filter(&email)
                {
                    format!(
                        "The following filter is configured for you: `{}`. It is **{}**.",
                        filter_str,
                        if filter_enabled {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    )
                } else {
                    "No filter is configured for you.".to_string()
                };

                vec![Task::Reply(Response::new(email, resp))]
            }
            Action::FilterAdd(email, filter) => {
                let resp = self.state.add_filter(&email, &filter).map(
                |()|
                "Filter successfully added and enabled."
            ).unwrap_or(
                "Your provided filter is invalid. Please double-check the regex you provided. Specifications of the regex are here: https://doc.rust-lang.org/regex/regex/index.html#syntax");
                vec![Task::Reply(Response::new(email, resp.to_string()))]
            }
            Action::FilterEnable(email, enable) => {
                let resp = self.state.enable_and_get_filter(&email, enable).map(
                |filter|
                if enable {
                format!(
                    "Filter successfully enabled. The following filter is configured: {}",
                    filter
                )
                } else {
                    "Filter successfully disabled.".to_string()
                }
            ).unwrap_or_else(|()|
                             if enable {
                                 "Cannot enable filter since there is none configured. User `filter <regex>` to add a new filter.".to_string()
                             } else {
                                 "No need to disable the filter since there is none configured.".to_string()
                             }
                );

                vec![Task::Save, Task::Reply(Response::new(email, resp))]
            }
            Action::ReviewerAdded(event) => self
                .get_reviewer_added_msg(&event)
                .map(|(user, message)| Task::Reply(Response::new(user.email().to_owned(), message)))
                .into_iter()
                .collect(),
        }
    }

    fn handle_task(&mut self, task: Task) -> Option<Response> {
        debug!("New task {:#?}", task);
        let response = match task {
            Task::Reply(response) => Some(response),
            Task::Save => {
                self.save("state.json")
                    .map_err(|err| {
                        error!("Could not save state: {:?}", err);
                    })
                    .ok();
                None
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
        let owner_email = spark::EmailRef::new(change.owner.email.as_ref()?);

        // try to find the use and check it is enabled
        let user = self
            .state
            .find_user_by_email(owner_email)
            .filter(|user| user.is_enabled())?;

        let is_human = !approver.to_lowercase().contains("bot");

        // filter all messages that were already sent to the user recently
        if !approvals.is_empty() && self.rate_limiter.limit(user, &*event) {
            debug!("Filtered approval due to cache hit.");
            return None;
        }

        self.formatter
            .format_comment_added(&event, is_human)
            .unwrap_or_else(|e| {
                error!("message formatting failed: {}", e);
                None
            })
            .filter(|msg| {
                // if user has configured and enabled a filter try to apply it
                !self.state.is_filtered(user, &msg)
            })
            .map(|m| (user, m, is_human))
    }

    fn get_reviewer_added_msg(
        &mut self,
        event: &gerrit::ReviewerAddedEvent,
    ) -> Option<(&User, String)> {
        let reviewer_email = spark::EmailRef::new(event.reviewer.email.as_ref()?);
        let user = self
            .state
            .find_user_by_email(reviewer_email)
            .filter(|user| user.is_enabled())?;

        // filter all messages that were already sent to the user recently
        if self.rate_limiter.limit(user, event) {
            debug!("Filtered reviewer-added due to cache hit.");
            return None;
        }

        let message = self.formatter.format_reviewer_added(event).ok()?;

        Some((user, message))
    }

    pub fn save<P>(&self, filename: P) -> Result<(), BotError>
    where
        P: AsRef<Path>,
    {
        let f = File::create(filename)?;
        serde_json::to_writer(f, &self.state)?;
        Ok(())
    }

    pub fn status_for(&self, email: &spark::EmailRef) -> String {
        let user = self.state.find_user(email);
        let enabled = user.map_or(false, |u| u.is_enabled());
        let enabled_user_count =
            self.state.users().filter(|u| u.is_enabled()).count() - if enabled { 1 } else { 0 };
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
    Enable(spark::Email),
    Disable(spark::Email),
    UpdateApprovals(Box<gerrit::CommentAddedEvent>),
    Help(spark::Email),
    Unknown(spark::Email),
    Status(spark::Email),
    Version(spark::Email),
    FilterStatus(spark::Email),
    FilterAdd(spark::Email, String /* filter */),
    FilterEnable(spark::Email, bool),
    ReviewerAdded(Box<gerrit::ReviewerAddedEvent>),
}

#[derive(Debug)]
pub struct Response {
    pub email: spark::Email,
    pub message: String,
}

impl Response {
    pub fn new<A>(email: spark::Email, message: A) -> Response
    where
        A: Into<String>,
    {
        Response {
            email,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub enum Task {
    Reply(Response),
    Save,
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

    use spark::EmailRef;

    use super::*;

    struct TestGerritCommandRunner;
    impl GerritCommandRunner for TestGerritCommandRunner {}

    #[derive(Clone)]
    struct TestSparkClient;

    type TestBot = Bot<TestGerritCommandRunner, TestSparkClient>;

    impl SparkClient for TestSparkClient {
        type ReplyFuture = future::FutureResult<(), spark::Error>;
        fn send_message(&self, _email: &EmailRef, _msg: &str) -> Self::ReplyFuture {
            future::ok(())
        }
    }

    impl TestBot {
        fn add_user(&mut self, email: &str) {
            self.state.add_user(EmailRef::new(email));
        }

        fn enable(&mut self, email: &str, enabled: bool) {
            self.state.enable(EmailRef::new(email), enabled);
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
        fn has_email(&mut self, expected: &str);
        fn is_enabled(&mut self);
        fn is_not_enabled(&mut self);
    }

    impl<'s> UserAssertions for spectral::Spec<'s, &User> {
        fn has_email(&mut self, expected: &str) {
            let actual = self.subject.email();
            let expected = EmailRef::new(expected);
            if actual != expected {
                spectral::AssertionFailure::from_spec(self)
                    .with_expected(format!("user with email <{}>", expected))
                    .with_actual(format!("<{}>", actual))
                    .fail();
            }
        }

        fn is_enabled(&mut self) {
            if !self.subject.is_enabled() {
                spectral::AssertionFailure::from_spec(self)
                    .with_expected("user is enabled".to_string())
                    .with_actual("it is not".to_string())
                    .fail();
            }
        }

        fn is_not_enabled(&mut self) {
            if self.subject.is_enabled() {
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
                bot.add_user("some@example.com");
            }

            before {
                assert_that!(bot.state.users().count()).is_equal_to(1);
            }

            it "has the expected attributes" {
                let user = bot.state.users().nth(0).unwrap();
                assert_that!(user).has_email("some@example.com");
                assert_that!(user).has_email("some@example.com");
                assert_that!(user).is_enabled();
            }

            test "enabled status response" {
                let resp = bot.status_for(EmailRef::new("some@example.com"));
                assert_that!(resp).contains("enabled");
            }

            test "disabled status response" {
                bot.enable("some@example.com", false);
                let resp = bot.status_for(EmailRef::new("some@example.com"));
                assert_that!(resp).contains("disabled");
            }

            test "existing user can be enabled" {
                bot.enable("some@example.com", true);
                let users: Vec<_> = bot.state.users().collect();
                assert_that!(users)
                    .has_item_matching(
                        |u| u.email() == EmailRef::new("some@example.com")
                            && u.is_enabled());
                assert_that!(bot.state.users().count()).is_equal_to(1);
            }

            test "existing can be disabled" {
                bot.enable("some@example.com", false);
                let users: Vec<_> = bot.state.users().collect();
                assert_that!(users)
                    .has_item_matching(
                        |u| u.email() == EmailRef::new("some@example.com")
                            && !u.is_enabled());
                assert_that!(bot.state.users().count()).is_equal_to(1);
            }
        }

        test "non-existing user is automatically added when enabled" {
            assert_that!(bot.state.users().count()).is_equal_to(0);
            let mut bot = bot;
            bot.enable("some@example.com", true);
            let users: Vec<_> = bot.state.users().collect();
            assert_that!(users)
                .has_item_matching(
                    |u| u.email() == EmailRef::new("some@example.com")

                        && u.is_enabled());
            assert_that!(bot.state.users().count()).is_equal_to(1);
        }

        test "non-existing user is automatically added when disabled" {
            assert_that!(bot.state.users().count()).is_equal_to(0);
            let mut bot = bot;
            bot.enable("some@example.com", false);
            let users: Vec<_> = bot.state.users().collect();
            assert_that!(users)
                .has_item_matching(
                    |u| u.email() == EmailRef::new("some@example.com")
                        && !u.is_enabled());
            assert_that!(bot.state.users().count()).is_equal_to(1);
        }

        test "unknown user gets disabled status response" {
            let resp = bot.status_for(EmailRef::new("some_non_existent_id"));
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
        bot.state.add_user(EmailRef::new("approver@example.com"));
        let res = bot.get_approvals_msg(Box::new(get_event()));
        assert!(res.is_none());
    }

    #[test]
    fn get_approvals_msg_for_user_with_disabled_notifications() {
        // the approval is for the user with disabled notifications
        // => no message
        let mut bot = new_bot();
        bot.state.add_user(EmailRef::new("author@example.com"));
        bot.enable("author@example.com", false);
        let res = bot.get_approvals_msg(Box::new(get_event()));
        assert!(res.is_none());
    }

    #[test]
    fn get_approvals_msg_for_user_with_enabled_notifications() {
        // the approval is for the user with enabled notifications
        // => message
        let mut bot = new_bot();
        bot.state.add_user(EmailRef::new("author@example.com"));
        let res = bot.get_approvals_msg(Box::new(get_event()));
        assert!(res.is_some());
        let (user, msg, is_human) = res.unwrap();
        assert_eq!(user.email(), EmailRef::new("author@example.com"));
        assert!(msg.contains("Some review."));
        assert!(is_human);
    }

    #[test]
    fn get_approvals_msg_for_user_with_enabled_notifications_and_filter() {
        // the approval is for the user with enabled notifications
        // => message
        let mut bot = new_bot();
        bot.state.add_user(EmailRef::new("author@example.com"));

        {
            let res = bot
                .state
                .add_filter(EmailRef::new("author@example.com"), ".*Code-Review.*");
            assert!(res.is_ok());
            let res = bot.get_approvals_msg(Box::new(get_event()));
            assert!(res.is_none());
        }
        {
            let res = bot
                .state
                .enable_and_get_filter(EmailRef::new("author@example.com"), false);
            assert!(res.is_ok());
            let res = bot.get_approvals_msg(Box::new(get_event()));
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.email(), EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
        {
            let res = bot
                .state
                .enable_and_get_filter(EmailRef::new("author@example.com"), true);
            assert!(res.is_ok());
            let res = bot.state.add_filter(
                EmailRef::new("author@example.com"),
                "some_non_matching_filter",
            );
            assert!(res.is_ok());
            let res = bot.get_approvals_msg(Box::new(get_event()));
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.email(), EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
    }

    #[test]
    fn get_approvals_msg_for_quickly_repeated_event() {
        // same approval for the user with enabled notifications 2 times in less than 1 sec
        // => first time get message, second time nothing
        let mut bot = new_bot_with_msg_cache(10, Duration::from_secs(1));
        bot.state.add_user(EmailRef::new("author@example.com"));
        {
            let res = bot.get_approvals_msg(Box::new(get_event()));
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.email(), EmailRef::new("author@example.com"));
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
        bot.state.add_user(EmailRef::new("author@example.com"));
        {
            let res = bot.get_approvals_msg(Box::new(get_event()));
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.email(), EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
            assert!(is_human);
        }
        thread::sleep(Duration::from_millis(200));
        {
            let res = bot.get_approvals_msg(Box::new(get_event()));
            assert!(res.is_some());
            let (user, msg, is_human) = res.unwrap();
            assert_eq!(user.email(), EmailRef::new("author@example.com"));
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
        bot.state.add_user(EmailRef::new("author@example.com"));
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
