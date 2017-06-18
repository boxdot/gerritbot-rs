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

fn format_approval_value(value: &str) -> String {
    let value: i8 = value.parse().unwrap_or(0);
    // TODO: when Spark will be allow to format text with different colors,
    // set green resp. red color here.
    if value > 0 {
        format!("+{}", value)
    } else {
        format!("{}", value)
    }
}

impl Bot {
    pub fn new() -> Bot {
        Bot { users: Vec::new() }
    }

    fn enable<'a>(&'a mut self, person_id: &str, email: &str, enabled: bool) -> &'a User {
        // FIXME: Replace linear search
        let pos = self.users.iter().position(
            |u| u.spark_person_id == person_id,
        );
        let user: &'a mut User = match pos {
            Some(pos) => &mut self.users[pos],
            None => {
                self.users.push(User::new(
                    String::from(person_id),
                    String::from(email),
                ));
                self.users.iter_mut().last().unwrap()
            }
        };
        user.enabled = enabled;
        user
    }

    fn get_approvals_msg(&self, event: gerrit::Event) -> Option<(&User, String)> {
        println!("[D] Incoming approvals: {:?}", event);

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
                                println!("Filtered approval: {:?}", !filtered);
                                filtered
                            })
                            .map(|approval| {
                                format!(
                                    "[{}]({}): {} ({}) from {}",
                                    change.subject,
                                    change.url,
                                    format_approval_value(&approval.value),
                                    approval.approval_type,
                                    approver
                                )
                            })
                            .collect();
                        return if !msgs.is_empty() {
                            Some((user, msgs.join("\n")))
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
}

#[derive(Debug)]
pub enum Action {
    Enable(spark::PersonId, spark::Email),
    Disable(spark::PersonId, spark::Email),
    UpdateApprovals(gerrit::Event),
    Help(spark::PersonId),
    Unknown(spark::PersonId),
    NoOp,
}

#[derive(Debug)]
pub struct Response {
    // TODO: Switch to a reference, since it should be alive inside of the Bot state
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
        _ => None,
    };
    (bot, task)
}
