use std::borrow::Cow;
use std::convert::{self, identity};
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
mod command;
mod format;
mod rate_limit;
mod state;
mod version;

use command::Command;
use format::Formatter;
pub use format::DEFAULT_FORMAT_SCRIPT;
use rate_limit::RateLimiter;
pub use state::State;
use state::{User, UserFlag, NOTIFICATION_FLAGS, REVIEW_COMMENT_FLAGS};
use version::VERSION_INFO;

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
    let sender = message.person_email;
    let text = message.text;

    match text.parse() {
        Ok(command) => Action::RunCommand { sender, command },
        Err(()) => Action::UnknownCommand { sender },
    }
}

/// Transform a gerrit event into a bot action.
fn gerrit_event_to_action(event: gerrit::Event) -> Option<Action> {
    match event {
        gerrit::Event::CommentAdded(event) => Some(Action::CommentAdded(Box::new(event))),
        gerrit::Event::ReviewerAdded(event) => Some(Action::ReviewerAdded(Box::new(event))),
        gerrit::Event::ChangeMerged(event) => Some(Action::ChangeMerged(Box::new(event))),
        gerrit::Event::ChangeAbandoned(event) => Some(Action::ChangeAbandoned(Box::new(event))),
    }
}

pub trait IsHuman {
    fn is_human(&self) -> bool;
}

impl IsHuman for gerrit::User {
    fn is_human(&self) -> bool {
        // XXX: Maybe this should be sophisticated to avoid matching humans
        // whose name contains "bot"?
        match self.username {
            Some(ref username) if username.contains("bot") => false,
            _ => true,
        }
    }
}

trait SparkEmail {
    fn spark_email(&self) -> Option<&spark::EmailRef>;
}

impl SparkEmail for gerrit::User {
    fn spark_email(&self) -> Option<&spark::EmailRef> {
        self.email.as_ref().map(|s| spark::EmailRef::new(s))
    }
}

pub fn request_extended_gerrit_info(event: &gerrit::Event) -> Cow<'static, [gerrit::ExtendedInfo]> {
    let mut extended_info = Vec::new();

    match event {
        gerrit::Event::CommentAdded(event) => {
            let owner_name = event.change.owner.username.as_ref();
            let approver_name = event.author.username.as_ref();

            if event.author.is_human() && maybe_has_inline_comments(event) {
                extended_info.push(gerrit::ExtendedInfo::InlineComments);
            }

            // Fetch existing reviewers, so they can be updated when the author
            // comments.
            if owner_name == approver_name {
                extended_info.push(gerrit::ExtendedInfo::AllApprovals);
            }

            // Could be smarter here by checking for old_value and if the value
            // is positive.
            extended_info.push(gerrit::ExtendedInfo::SubmitRecords);
        }
        gerrit::Event::ChangeMerged(_) | gerrit::Event::ChangeAbandoned(_) => {
            extended_info.push(gerrit::ExtendedInfo::AllApprovals);
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
            Action::RunCommand { sender, command } => self.run_command(sender, command),
            Action::UnknownCommand { sender } => self
                .formatter
                .format_greeting()
                .map_err(|e| error!("failed to format message: {}", e))
                .ok()
                .into_iter()
                .flatten()
                .map(|message| Task::Reply(Response::new(sender.clone(), message)))
                .collect(),
            Action::CommentAdded(event) => self
                .get_comment_messages(event)
                .into_iter()
                .map(|(email, message)| Task::Reply(Response::new(email, message)))
                .collect(),
            Action::ReviewerAdded(event) => self
                .get_reviewer_added_msg(&event)
                .map(|(user, message)| Task::Reply(Response::new(user.email().to_owned(), message)))
                .into_iter()
                .collect(),
            Action::ChangeMerged(event) => self
                .get_change_merged_messages(&event)
                .into_iter()
                .map(|(email, message)| Task::Reply(Response::new(email, message)))
                .into_iter()
                .collect(),
            Action::ChangeAbandoned(event) => self
                .get_change_abandoned_messages(&event)
                .into_iter()
                .map(|(email, message)| Task::Reply(Response::new(email, message)))
                .into_iter()
                .collect(),
        }
    }

    fn run_command(&mut self, sender: spark::Email, command: Command) -> Vec<Task> {
        match command {
            Command::Enable => {
                self.state.enable(&sender, true);
                vec![
                    Task::Save,
                    Task::Reply(Response::new(sender, "Got it! Happy reviewing!")),
                ]
            }
            Command::Disable => {
                self.state.enable(&sender, false);
                vec![
                    Task::Save,
                    Task::Reply(Response::new(sender, "Got it! I will stay silent.")),
                ]
            }
            Command::Help => self
                .formatter
                .format_help()
                .map_err(|e| error!("failed to format help: {}", e))
                .ok()
                .into_iter()
                .flatten()
                .map(|message| Task::Reply(Response::new(sender.clone(), message)))
                .collect(),
            Command::Version => self
                .formatter
                .format_message(None, &VERSION_INFO)
                .map_err(|e| error!("failed to format version: {}", e))
                .ok()
                .and_then(identity)
                .map(|version_message| Task::Reply(Response::new(sender, version_message)))
                .into_iter()
                .collect(),
            Command::Status => self
                .status_for(&sender)
                .map(|status| Task::Reply(Response::new(sender, status)))
                .into_iter()
                .collect(),
            Command::FilterStatus => {
                let resp =
                    if let Some((filter_str, filter_enabled)) = self.state.get_filter(&sender) {
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

                vec![Task::Reply(Response::new(sender, resp))]
            }
            Command::FilterAdd(filter) => {
                let resp = self.state.add_filter(&sender, &filter).map(
                |()|
                "Filter successfully added and enabled."
            ).unwrap_or(
                "Your provided filter is invalid. Please double-check the regex you provided. Specifications of the regex are here: https://doc.rust-lang.org/regex/regex/index.html#syntax");
                vec![Task::Reply(Response::new(sender, resp.to_string()))]
            }
            Command::FilterEnable(enable) => {
                let resp = self.state.enable_and_get_filter(&sender, enable).map(
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

                vec![Task::Save, Task::Reply(Response::new(sender, resp))]
            }
            Command::SetFlag(flag, enable) => {
                self.state.set_flag(&sender, flag, enable);
                vec![
                    Task::Save,
                    Task::Reply(Response::new(
                        sender,
                        format!(
                            "Flag {} **{}**",
                            flag,
                            if enable { "enabled" } else { "disabled" }
                        ),
                    )),
                ]
            }
        }
    }

    fn handle_task(&mut self, task: Task) -> Option<Response> {
        debug!("New task {:#?}", task);
        match task {
            Task::Reply(response) => Some(response),
            Task::Save => {
                self.save("state.json")
                    .map_err(|err| {
                        error!("Could not save state: {:?}", err);
                    })
                    .ok();
                None
            }
        }
    }

    /// Return iterator of users which might be interested in an event.
    fn interested_users<'bot, 'event, 'result>(
        &'bot self,
        change: &'event gerrit::Change,
        patchset: &'event gerrit::Patchset,
    ) -> impl Iterator<Item = &'bot User> + 'result
    where
        'bot: 'result,
        'event: 'result,
    {
        // TODO: this function currently only considers users that added an
        // approval on the patchset in question. Ideally, users that approved
        // (or event commented) on any patchset might be considered interested.
        // For that to work with the current model for each event we'd have to
        // query gerrit with the --all-reviewers flag to get all reviewers of a
        // change (or even --patch-sets to get all patchsets) as well. This
        // might create quite some load on the gerrit server. We can already get
        // almost all of that information by subscribing to events. This would
        // holding more state and tracking changes, reviewers etc.
        let _ = change;
        patchset
            .approvals
            .iter()
            .flatten()
            .filter_map(|approval| approval.by.as_ref())
            .chain(std::iter::once(&change.owner))
            .filter(|user| user.is_human())
            .filter_map(|user| user.spark_email())
            .filter_map(move |email| self.state.find_user_by_email(email))
    }

    fn get_comment_response_messages(
        &self,
        event: Box<gerrit::CommentAddedEvent>,
    ) -> Vec<(spark::Email, String)> {
        self.interested_users(&event.change, &event.patchset)
            .filter(|user| Some(user.email()) != event.author.spark_email())
            .filter(|user| user.has_flag(UserFlag::NotifyReviewResponses))
            .filter_map(|user| {
                self.formatter
                    .format_message(Some(user), &*event)
                    .map_err(|e| error!("message formatting failed: {}", e))
                    .unwrap_or(None)
                    .filter(|message| !self.state.is_filtered(user, &message))
                    .map(|message| (user.email().to_owned(), message))
            })
            .collect()
    }

    fn get_approvals_msg(
        &mut self,
        event: Box<gerrit::CommentAddedEvent>,
    ) -> Option<(spark::Email, String)> {
        let approvals = event
            .approvals
            .as_ref()
            .map(Vec::as_slice)
            .unwrap_or(&[][..]);
        let owner_email = event.change.owner.spark_email()?;

        // try to find the user and check it is enabled
        let user = self
            .state
            .find_user_by_email(owner_email)
            .filter(|user| user.has_any_flag(REVIEW_COMMENT_FLAGS))?;

        // filter all messages that were already sent to the user recently
        if !approvals.is_empty() && self.rate_limiter.limit(user, &*event) {
            debug!("Filtered approval due to cache hit.");
            return None;
        }

        self.formatter
            .format_message(Some(user), &*event)
            .unwrap_or_else(|e| {
                error!("message formatting failed: {}", e);
                None
            })
            .filter(|msg| {
                // if user has configured and enabled a filter try to apply it
                !self.state.is_filtered(user, &msg)
            })
            .map(|m| (owner_email.to_owned(), m))
    }

    fn get_comment_messages(
        &mut self,
        event: Box<gerrit::CommentAddedEvent>,
    ) -> Vec<(spark::Email, String)> {
        debug!("Incoming approvals: {:#?}", event);
        let owner_email = event.change.owner.spark_email();
        let approver_email = event.author.spark_email();

        if owner_email == approver_email {
            self.get_comment_response_messages(event)
        } else {
            self.get_approvals_msg(event).into_iter().collect()
        }
    }

    fn get_reviewer_added_msg(
        &mut self,
        event: &gerrit::ReviewerAddedEvent,
    ) -> Option<(&User, String)> {
        let reviewer_email = spark::EmailRef::new(event.reviewer.email.as_ref()?);
        let user = self
            .state
            .find_user_by_email(reviewer_email)
            .filter(|user| user.has_flag(UserFlag::NotifyReviewerAdded))?;

        // filter all messages that were already sent to the user recently
        if self.rate_limiter.limit(user, event) {
            debug!("Filtered reviewer-added due to cache hit.");
            return None;
        }

        let message = self
            .formatter
            .format_message(Some(user), event)
            .map_err(|e| error!("formatting reviewer added failed: {}", e))
            .ok()??;

        Some((user, message))
    }

    fn get_change_merged_messages(
        &mut self,
        event: &gerrit::ChangeMergedEvent,
    ) -> Vec<(spark::Email, String)> {
        self.interested_users(&event.change, &event.patchset)
            .filter(|user| event.submitter.spark_email() != Some(user.email()))
            .filter(|user| user.has_flag(UserFlag::NotifyChangeMerged))
            .filter_map(|user| {
                self.formatter
                    .format_message(Some(user), event)
                    .map_err(|e| error!("message formatting failed: {}", e))
                    .ok()
                    .and_then(identity)
                    .filter(|message| !self.state.is_filtered(user, &message))
                    .map(|message| (user.email().to_owned(), message))
            })
            .collect()
    }

    fn get_change_abandoned_messages(
        &mut self,
        event: &gerrit::ChangeAbandonedEvent,
    ) -> Vec<(spark::Email, String)> {
        self.interested_users(&event.change, &event.patchset)
            .filter(|user| event.abandoner.spark_email() != Some(user.email()))
            .filter(|user| user.has_flag(UserFlag::NotifyChangeAbandoned))
            .filter_map(|user| {
                self.formatter
                    .format_message(Some(user), event)
                    .map_err(|e| error!("message formatting failed: {}", e))
                    .ok()
                    .and_then(identity)
                    .filter(|message| !self.state.is_filtered(user, &message))
                    .map(|message| (user.email().to_owned(), message))
            })
            .collect()
    }

    pub fn save<P>(&self, filename: P) -> Result<(), BotError>
    where
        P: AsRef<Path>,
    {
        let f = File::create(filename)?;
        serde_json::to_writer(f, &self.state)?;
        Ok(())
    }

    fn status_for(&self, email: &spark::EmailRef) -> Option<String> {
        let user = self.state.find_user(email);
        let enabled_user_count = self
            .state
            .users()
            .filter(|u| u.has_any_flag(NOTIFICATION_FLAGS))
            .count();
        self.formatter
            .format_status(user, enabled_user_count)
            .map_err(|e| error!("formatting status failed: {}", e))
            .ok()?
    }
}

#[derive(Debug)]
enum Action {
    RunCommand {
        sender: spark::Email,
        command: Command,
    },
    UnknownCommand {
        sender: spark::Email,
    },
    CommentAdded(Box<gerrit::CommentAddedEvent>),
    ReviewerAdded(Box<gerrit::ReviewerAddedEvent>),
    ChangeMerged(Box<gerrit::ChangeMergedEvent>),
    ChangeAbandoned(Box<gerrit::ChangeAbandonedEvent>),
}

#[derive(Debug)]
struct Response {
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
enum Task {
    Reply(Response),
    Save,
}

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
    use std::borrow::Borrow;
    use std::fmt::Debug;
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
        fn has_any_flag<I, F>(&self, flags: I)
        where
            I: IntoIterator<Item = F> + Debug + Clone,
            F: Borrow<UserFlag>;
        fn has_no_flag<I, F>(&self, flags: I)
        where
            I: IntoIterator<Item = F> + Debug + Clone,
            F: Borrow<UserFlag>;
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

        fn has_any_flag<I, F>(&self, flags: I)
        where
            I: IntoIterator<Item = F> + Debug + Clone,
            F: Borrow<UserFlag>,
        {
            if !self.subject.has_any_flag(flags.clone()) {
                spectral::AssertionFailure::from_spec(self)
                    .with_expected(format!("user has at least one of the flags {:?}", flags))
                    .with_actual("it has none of the given flags".to_string())
                    .fail();
            }
        }

        fn has_no_flag<I, F>(&self, flags: I)
        where
            I: IntoIterator<Item = F> + Debug + Clone,
            F: Borrow<UserFlag>,
        {
            if self.subject.has_any_flag(flags.clone()) {
                spectral::AssertionFailure::from_spec(self)
                    .with_expected(format!("user has none of the flags {:?}", flags))
                    .with_actual("it has at least one of the given flags".to_string())
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
                assert_that!(user).has_any_flag(NOTIFICATION_FLAGS);
            }

            test "enabled status response" {
                let resp = bot.status_for(EmailRef::new("some@example.com"));
                assert_that!(resp).is_some().contains("enabled");
            }

            test "disabled status response" {
                bot.enable("some@example.com", false);
                let resp = bot.status_for(EmailRef::new("some@example.com"));
                assert_that!(resp).is_some().contains("disabled");
            }

            test "existing user can be enabled" {
                bot.enable("some@example.com", true);
                let users: Vec<_> = bot.state.users().collect();
                assert_that!(users)
                    .has_item_matching(
                        |u| u.email() == EmailRef::new("some@example.com")
                            && u.has_any_flag(NOTIFICATION_FLAGS));
                assert_that!(bot.state.users().count()).is_equal_to(1);
            }

            test "existing can be disabled" {
                bot.enable("some@example.com", false);
                let users: Vec<_> = bot.state.users().collect();
                assert_that!(users)
                    .has_item_matching(
                        |u| u.email() == EmailRef::new("some@example.com")
                            && !u.has_any_flag(NOTIFICATION_FLAGS));
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

                        && u.has_any_flag(NOTIFICATION_FLAGS));
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
                        && !u.has_any_flag(NOTIFICATION_FLAGS));
            assert_that!(bot.state.users().count()).is_equal_to(1);
        }

        test "unknown user gets disabled status response" {
            let resp = bot.status_for(EmailRef::new("some_non_existent_id"));
            assert_that!(resp).is_some().contains("disabled");
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
        let (email, msg) = res.unwrap();
        assert_eq!(email, EmailRef::new("author@example.com"));
        assert!(msg.contains("Some review."));
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
            let (user, msg) = res.unwrap();
            assert_eq!(user, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
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
            let (email, msg) = res.unwrap();
            assert_eq!(email, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
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
            let (email, msg) = res.unwrap();
            assert_eq!(email, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
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
            let (email, msg) = res.unwrap();
            assert_eq!(email, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
        }
        thread::sleep(Duration::from_millis(200));
        {
            let res = bot.get_approvals_msg(Box::new(get_event()));
            assert!(res.is_some());
            let (email, msg) = res.unwrap();
            assert_eq!(email, EmailRef::new("author@example.com"));
            assert!(msg.contains("Some review."));
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
