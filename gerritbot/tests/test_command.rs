use std::cell::RefCell;
use std::rc::Rc;

use futures::{future, stream, Future};
use lazy_static::lazy_static;

use spectral::prelude::*;
use speculate::speculate;

use gerritbot_spark as spark;
use spark::{EmailRef, PersonId, PersonIdRef};

use gerritbot::*;

#[derive(Debug, Clone, Default)]
struct TestGerritCommandRunner;
impl GerritCommandRunner for TestGerritCommandRunner {}

#[derive(Debug, Clone)]
struct Reply {
    person_id: PersonId,
    message: String,
}

type Replies = Rc<RefCell<Vec<Reply>>>;

#[derive(Debug, Clone, Default)]
struct TestSparkClient {
    replies: Replies,
}

impl SparkClient for TestSparkClient {
    type ReplyFuture = future::FutureResult<(), spark::Error>;
    fn send_message(&self, person_id: &PersonId, msg: &str) -> Self::ReplyFuture {
        self.replies.borrow_mut().push(Reply {
            person_id: person_id.to_owned(),
            message: msg.to_string(),
        });
        future::ok(())
    }
}

type TestBot = Bot<TestGerritCommandRunner, TestSparkClient>;

lazy_static! {
    static ref TEST_PERSON_ID: &'static PersonIdRef = PersonIdRef::new("test_person_id");
    static ref TEST_PERSON_EMAIL: &'static EmailRef = EmailRef::new("test@person.test");
}

trait TestBotTrait: Sized {
    fn new() -> (Self, Replies);
    fn send_message(self, message: &str);
    fn send_messages(self, messages: &[&str]);
}

impl TestBotTrait for TestBot {
    fn new() -> (Self, Replies) {
        let replies = Replies::default();
        let bot = Builder::new(State::new()).build(
            Default::default(),
            TestSparkClient {
                replies: replies.clone(),
            },
        );
        (bot, replies)
    }

    fn send_messages(self, messages: &[&str]) {
        let spark_messages = stream::iter_ok(messages.iter().map(|msg| spark::Message {
            person_email: TEST_PERSON_EMAIL.to_owned(),
            person_id: TEST_PERSON_ID.to_owned(),
            text: msg.to_string(),
            ..Default::default()
        }));
        let gerrit_events = stream::empty();
        self.run(gerrit_events, spark_messages).wait().unwrap();
    }

    fn send_message(self, message: &str) {
        self.send_messages(&[message][..]);
    }
}

speculate! {
    before {
        let (bot, replies) = TestBot::new();
    }

    describe "command tests" {
        test "status" {
            bot.send_message("status");
            let replies = Rc::try_unwrap(replies).unwrap().into_inner();
            assert_that!(replies).has_length(1);
            assert_that!(replies[0].person_id).is_equal_to(TEST_PERSON_ID.to_owned());
            assert_that!(replies[0].message).contains("disabled");
        }

        test "enable then status" {
            bot.send_messages(&["enable", "status"][..]);
            let replies = Rc::try_unwrap(replies).unwrap().into_inner();
            assert_that!(replies).has_length(2);
            assert_that!(replies[0].person_id).is_equal_to(TEST_PERSON_ID.to_owned());
            assert_that!(replies[0].message).contains("Happy reviewing!");
            assert_that!(replies[1].person_id).is_equal_to(TEST_PERSON_ID.to_owned());
            assert_that!(replies[1].message).contains("enabled");
        }

        test "unknown command" {
            bot.send_message("this is not a known command");
            let replies = Rc::try_unwrap(replies).unwrap().into_inner();
            assert_that!(replies).has_length(1);
            assert_that!(replies[0].person_id).is_equal_to(TEST_PERSON_ID.to_owned());
            assert_that!(replies[0].message).contains("I am GerritBot");
        }

        test "version" {
            bot.send_message("version");
            let replies = Rc::try_unwrap(replies).unwrap().into_inner();
            assert_that!(replies).has_length(1);
            assert_that!(replies[0].person_id).is_equal_to(TEST_PERSON_ID.to_owned());
            assert_that!(replies[0].message).contains(env!("CARGO_PKG_NAME"));
            assert_that!(replies[0].message).contains(env!("CARGO_PKG_VERSION"));
            assert_that!(replies[0].message).contains(env!("VERGEN_SHA"));
        }
    }
}
