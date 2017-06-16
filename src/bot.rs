use std::path::Path;
use std::io;
use std::fs::File;
use std::convert;

use chrono;
use serde_json;
use sha2::{self, Digest};

use gerrit;
use spark;
use utils;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    spark_person_id: spark::PersonId,
    gerrit_username: gerrit::Username,
    verified: bool,
    verification_token: String,
    enabled: bool,
}

fn generate_verification_token(person_id: &str, username: &str) -> String {
    let now = chrono::UTC::now();
    let salt = utils::xorshift64star(now.timestamp() as u64);
    calc_verification_token(person_id, username, salt)
}

fn calc_verification_token(person_id: &str, username: &str, salt: u64) -> String {
    let mut hasher = sha2::Sha256::default();
    hasher.input(person_id.as_bytes());
    hasher.input(username.as_bytes());
    hasher.input(&utils::transform_u64_to_array_of_u8(salt));
    format!("{:x}", hasher.result())
}

impl User {
    fn new(person_id: spark::PersonId, username: gerrit::Username) -> User {
        let token = generate_verification_token(&person_id, &username);
        User {
            spark_person_id: person_id,
            gerrit_username: username,
            verified: false,
            verification_token: token,
            enabled: true,
        }
    }
}

/// Describes a state of the bot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bot {
    gerrit_username: Option<String>, // TODO: this should not be part of the state
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

enum UserUpdate<'a> {
    NoOp(&'a User),
    Added(&'a User),
    Updated(&'a User),
}

impl Bot {
    pub fn new(gerrit_username: gerrit::Username) -> Bot {
        Bot {
            gerrit_username: Some(gerrit_username),
            users: Vec::new(),
        }
    }

    pub fn default() -> Bot {
        Bot {
            gerrit_username: None,
            users: Vec::new(),
        }
    }

    /// Return value is the user, and whether the user is a new one.
    fn configure<'a>(
        &'a mut self,
        person_id: spark::PersonId,
        username: gerrit::Username,
    ) -> UserUpdate<'a> {
        if let Some(pos) = self.users.iter().position(
            |u| u.spark_person_id == person_id,
        )
        {
            let user: &'a mut User = &mut self.users[pos];
            if user.gerrit_username != username {
                // User is trying to configure a different gerrit username => reset user
                let token = generate_verification_token(&person_id, &username);
                user.gerrit_username = username;
                user.verified = false;
                user.verification_token = token;
                UserUpdate::Updated(user)
            } else {
                UserUpdate::NoOp(user)
            }
        } else {
            self.users.push(User::new(person_id, username));
            UserUpdate::Added(self.users.last().unwrap())
        }
    }

    fn enable<'a>(&'a mut self, person_id: &str, enabled: bool) -> Option<&'a User> {
        let pos = self.users.iter().position(
            |u| u.spark_person_id == person_id,
        );
        match pos {
            Some(pos) => {
                let user: &'a mut User = &mut self.users[pos];
                user.enabled = enabled;
                Some(user)
            }
            None => None,
        }
    }

    /// Try to verify a user with given Gerrit username.
    ///
    /// Return Some(person_id) if verification was successful, otherwise None.
    fn verify(&mut self, username: gerrit::Username, token: String) -> Option<&User> {
        // TODO: linear search is slow
        for user in &mut self.users.iter_mut() {
            if user.gerrit_username == username && user.verification_token == token.trim() {
                user.verified = true;
                return Some(user);
            }
        }
        None
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

        // TODO: Fix linear search
        let users = &self.users;
        for user in users.iter() {
            if user.gerrit_username == change.owner.username {
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
                            let value: i8 = approval.value.parse().unwrap_or(0);
                            format!(
                                "{}: {}{} ({}) from {}",
                                change.subject,
                                if value > 0 { "+" } else { "" },
                                value,
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

    pub fn gerrit_username(&self) -> &str {
        match self.gerrit_username {
            Some(ref username) => username,
            None => "unknown",
        }
    }
}

#[derive(Debug)]
pub enum Action {
    Configure(spark::PersonId, gerrit::Username),
    Enable(spark::PersonId),
    Disable(spark::PersonId),
    Verify(gerrit::Username, String),
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

Before I can start notifying you, you need to configure your Gerrit yourname. For more information, type in **help**.

By the way, my icon is made by
[ Madebyoliver ](http://www.flaticon.com/authors/madebyoliver)
from
[ www.flaticon.com ](http://www.flaticon.com)
and is licensed by
[ CC 3.0 BY](http://creativecommons.org/licenses/by/3.0/).
"#;

const HELP_MSG: &'static str = r#"Commands:

`configure <gerrit_username>` Before I can start notifying you, I need to know your **Gerrit** username.

`enable` I will start notifying you.

`disable` I will stop notifying you.

`status` I will tell if notification are enabled or disabled for you.

`help` This message
"#;

macro_rules! verification_msg {
    () => (r#"Got it!

We are almost there. I still need to link your Spark account with your Gerrit account `{}`. For that, please create a new _draft_ in Gerrit with the folling commit message:

`{}`

and add me (username: `{}`) to the draft.

After the draft is created and I am added to it, I will get a message from Gerrit, and will notify you, that your accounts are linked.
"#;)
}

macro_rules! update_verification_msg {
    () => (r#"Got it!

You have already linked your Spark account previously with a different Gerrit username. I updated your username to `{}`, but now you have to link your accounts again.

Please create a new draft in Gerrit with the folling commit message:

`{}`

and add me (username: `{}`) to the draft.
"#;)
}

macro_rules! verification_pending_msg {
    () => (r#"Got it!

A verification for your Gerrit account `{}` is still pending. To complete verification, please create a new draft in Gerrit with the folling commit message:

`{}`

and add me (username: `{}`) to the draft.
"#;)
}

macro_rules! successfully_verified_msg {
    () => (r#"You successfully verified your Gerrit username `{}`. From now on, I will notify you about new +1/-1's. If you want to stop receiving notifications, use `disable` command.

Happy reviewing!
"#;)
}

const ALREADY_VERIFIED_MSG: &'static str =
    r#"Your Spark account already linked with Gerrit. Nothing to do!"#;

const NOT_CONFIGURED_MSG: &'static str =
    r#"Sorry, your account is not configured. Cf. **configure** and **help**."#;

/// Action controller
/// Return new bot state and an optional message to send to the user
pub fn update(action: Action, bot: Bot) -> (Bot, Option<Task>) {
    let mut bot = bot;
    let task = match action {
        Action::Configure(person_id, username) => {
            let bot_gerrit_username = String::from(bot.gerrit_username());
            let user_update = bot.configure(person_id, username);
            let task = match user_update {
                UserUpdate::Added(user) => {
                    Task::ReplyAndSave(Response::new(
                        user.spark_person_id.clone(),
                        format!(
                            verification_msg!(),
                            user.gerrit_username,
                            user.verification_token,
                            bot_gerrit_username
                        ),
                    ))
                }
                UserUpdate::Updated(user) => {
                    Task::ReplyAndSave(Response::new(
                        user.spark_person_id.clone(),
                        format!(
                            update_verification_msg!(),
                            user.gerrit_username,
                            user.verification_token,
                            bot_gerrit_username
                        ),
                    ))
                }
                UserUpdate::NoOp(user) => {
                    let msg = if user.verified {
                        String::from(ALREADY_VERIFIED_MSG)
                    } else {
                        format!(
                            verification_pending_msg!(),
                            user.gerrit_username,
                            user.verification_token,
                            bot_gerrit_username
                        )
                    };
                    Task::Reply(Response::new(user.spark_person_id.clone(), msg))
                }
            };
            Some(task)
        }
        Action::Enable(person_id) => {
            let successful = bot.enable(&person_id, true).map_or(
                false,
                |user| user.verified,
            );
            let task = if successful {
                Task::ReplyAndSave(Response::new(person_id, String::from("Got it!")))
            } else {
                Task::Reply(Response::new(person_id, String::from(NOT_CONFIGURED_MSG)))
            };
            Some(task)
        }
        Action::Disable(person_id) => {
            let successful = bot.enable(&person_id, false).map_or(
                false,
                |user| user.verified,
            );
            let task = if successful {
                Task::ReplyAndSave(Response::new(person_id, String::from("Got it!")))
            } else {
                Task::Reply(Response::new(person_id, String::from(NOT_CONFIGURED_MSG)))
            };
            Some(task)
        }
        Action::Verify(username, subject) => {
            bot.verify(username, subject).map(|user| {
                let msg = format!(successfully_verified_msg!(), user.gerrit_username);
                Task::ReplyAndSave(Response::new(user.spark_person_id.clone(), msg))
            })
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

#[cfg(test)]
mod test {
    use super::calc_verification_token;

    #[test]
    fn test_calc_verification_token() {
        let token = "2fab9448788184fe14299b04fbccb1c7c62101b6a265c29cf2e67a8d84406064";

        assert!(&calc_verification_token("some_person", "some_username", 0) == token);
        assert!(&calc_verification_token("some_other_person", "some_username", 0) != token);
        assert!(&calc_verification_token("some_person", "some_other_username", 0) != token);
        for salt in 1..2u64.pow(12) {
            assert!(&calc_verification_token("some_person", "some_username", salt) != token);
        }
    }
}
