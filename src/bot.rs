use std::path::Path;
use std::io;
use std::fs::File;
use std::convert;

use serde_json;

use gerrit;
use spark;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    spark_person_id: spark::PersonId,
    /// email of the user; assumed to be the same in Spark and Gerrit
    email: spark::Email,
    enabled: bool,
}

impl User {
    fn new(person_id: spark::PersonId, email: spark::Email) -> User {
        User {
            spark_person_id: person_id,
            email: email,
            enabled: true,
        }
    }
}

/// Describes a state of the bot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bot {
    users: Vec<User>,
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

fn format_approval_value(value: &str, approval_type: &str) -> String {
    let value: i8 = value.parse().unwrap_or(0);
    let sign = if value > 0 { "+" } else { "" };
    let icon = if approval_type.contains("WaitForVerification") {
        "⌛"
    } else if value > 0 {
        "👍"
    } else if value == 0 {
        "👉"
    } else {
        "👎"
    };

    // TODO: when Spark will allow to format text with different colors, set
    // green resp. red color here.
    format!("{} {}{}", icon, sign, value)
}

impl Bot {
    pub fn new() -> Bot {
        Bot { users: Vec::new() }
    }

    fn add_user<'a>(&'a mut self, person_id: &str, email: &str) -> &'a mut User {
        self.users.push(User::new(
            String::from(person_id),
            String::from(email),
        ));
        self.users.last_mut().unwrap()
    }

    fn find_or_add_user<'a>(&'a mut self, person_id: &str, email: &str) -> &'a mut User {
        let pos = self.users.iter().position(
            |u| u.spark_person_id == person_id,
        );
        let user: &'a mut User = match pos {
            Some(pos) => &mut self.users[pos],
            None => self.add_user(person_id, email),
        };
        user
    }

    fn enable<'a>(&'a mut self, person_id: &str, email: &str, enabled: bool) -> &'a User {
        let user: &'a mut User = self.find_or_add_user(person_id, email);
        user.enabled = enabled;
        user
    }

    fn get_approvals_msg(&self, event: gerrit::Event) -> Option<(&User, String)> {
        debug!("Incoming approvals: {:?}", event);

        let author = event.author;
        let change = event.change;
        let approvals = event.approvals;

        let approver = author.unwrap().username.clone();
        if approver == change.owner.username {
            // No need to notify about user's own approvals.
            return None;
        }

        if let Some(ref owner_email) = change.owner.email {
            // TODO: Fix linear search
            let users = &self.users;
            for user in users.iter() {
                if &user.email == owner_email {
                    if !user.enabled {
                        break;
                    }

                    if let Some(approvals) = approvals {
                        let msgs: Vec<String> = approvals
                            .iter()
                            .filter(|approval| {
                                let filtered = if let Some(ref old_value) = approval.old_value {
                                    old_value != &approval.value && approval.value != "0"
                                } else {
                                    approval.value != "0"
                                };
                                debug!("Filtered approval: {:?}", !filtered);
                                filtered
                            })
                            .map(|approval| {
                                format!(
                                    "[{}]({}) {} ({}) from {}",
                                    change.subject,
                                    change.url,
                                    format_approval_value(&approval.value, &approval.approval_type),
                                    approval.approval_type,
                                    approver
                                )
                            })
                            .collect();
                        return if !msgs.is_empty() {
                            Some((user, msgs.join("\n\n"))) // two newlines since it is markdown
                        } else {
                            None
                        };
                    }
                }
            }
        }
        None
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
        let bot: Bot = serde_json::from_reader(f)?;
        Ok(bot)
    }

    pub fn num_users(&self) -> usize {
        self.users.len()
    }

    pub fn status_for(&self, person_id: &str) -> String {
        let user = self.users.iter().find(|u| u.spark_person_id == person_id);
        let enabled = user.map_or(false, |u| u.enabled);
        format!(
            "Notifications for you are **{}**. Besides you, I am notifying another {} user(s).",
            if enabled { "enabled" } else { "disabled" },
            self.num_users() - 1
        )
    }
}

#[derive(Debug)]
pub enum Action {
    Enable(spark::PersonId, spark::Email),
    Disable(spark::PersonId, spark::Email),
    UpdateApprovals(gerrit::Event),
    Help(spark::PersonId),
    Unknown(spark::PersonId),
    Status(spark::PersonId),
    NoOp,
}

#[derive(Debug)]
pub struct Response {
    pub person_id: spark::PersonId,
    pub message: String,
}

impl Response {
    pub fn new(person_id: spark::PersonId, message: String) -> Response {
        Response {
            person_id: person_id,
            message: message,
        }
    }
}

#[derive(Debug)]
pub enum Task {
    Reply(Response),
    ReplyAndSave(Response),
}

const GREETINGS_MSG: &'static str =
    r#"Hi. I am GerritBot (**in a very early alpha**). I can watch Gerrit reviews for you, and notify you about new +1/-1's.

To enable notifications, just type in 'enable'. A small note: your email in Spark and in Gerrit has to be the same. Otherwise, I can't match your accounts.

For more information, type in **help**.

By the way, my icon is made by
[ Madebyoliver ](http://www.flaticon.com/authors/madebyoliver)
from
[ www.flaticon.com ](http://www.flaticon.com)
and is licensed by
[ CC 3.0 BY](http://creativecommons.org/licenses/by/3.0/).
"#;

const HELP_MSG: &'static str = r#"Commands:

`enable` I will start notifying you.

`disable` I will stop notifying you.

`status` Show if I am notifying you, and a little bit more information. 😉

`help` This message
"#;

/// Action controller
/// Return new bot state and an optional message to send to the user
pub fn update(action: Action, bot: Bot) -> (Bot, Option<Task>) {
    let mut bot = bot;
    let task = match action {
        Action::Enable(person_id, email) => {
            bot.enable(&person_id, &email, true);
            let task = Task::ReplyAndSave(Response::new(
                person_id,
                String::from("Got it! Happy reviewing!"),
            ));
            Some(task)
        }
        Action::Disable(person_id, email) => {
            bot.enable(&person_id, &email, false);
            let task = Task::ReplyAndSave(Response::new(
                person_id,
                String::from("Got it! I will stay silent."),
            ));
            Some(task)
        }
        Action::UpdateApprovals(event) => {
            bot.get_approvals_msg(event).map(|(user, message)| {
                Task::Reply(Response::new(user.spark_person_id.clone(), message))
            })
        }
        Action::Help(person_id) => {
            Some(Task::Reply(
                Response::new(person_id, String::from(HELP_MSG)),
            ))
        }
        Action::Unknown(person_id) => {
            Some(Task::Reply(
                Response::new(person_id, String::from(GREETINGS_MSG)),
            ))
        }
        Action::Status(person_id) => {
            let status = bot.status_for(&person_id);
            Some(Task::Reply(Response::new(person_id, status)))
        }
        _ => None,
    };
    (bot, task)
}

#[cfg(test)]
mod test {
    use super::{Bot, User};

    #[test]
    fn test_add_user() {
        let mut bot = Bot::new();
        bot.add_user("some_person_id", "some@example.com");
        assert_eq!(bot.users.len(), 1);
        assert_eq!(bot.users[0].spark_person_id, "some_person_id");
        assert_eq!(bot.users[0].email, "some@example.com");
        assert!(bot.users[0].enabled);
    }

    #[test]
    fn test_status_for() {
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
                    .position(|u| {
                        u.spark_person_id == "some_person_id" && u.email == "some@example.com" &&
                            u.enabled == enable
                    })
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
            bot.users.push(User::new(
                String::from("some_person_id"),
                String::from("some@example.com"),
            ));
            let num_users = bot.num_users();

            bot.enable("some_person_id", "some@example.com", enable);
            assert!(
                bot.users
                    .iter()
                    .position(|u| {
                        u.spark_person_id == "some_person_id" && u.email == "some@example.com" &&
                            u.enabled == enable
                    })
                    .is_some()
            );
            assert!(bot.num_users() == num_users);
        };
        test(true);
        test(false);
    }
}
