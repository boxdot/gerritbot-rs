use rustc_serialize::hex::ToHex;

use chrono;
use sha2::{self, Digest};

use gerrit;
use spark;
use utils;

#[derive(Debug)]
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
    calc_verification_token(&person_id, &username, salt)
}

// TODO: write a good test for it
fn calc_verification_token(person_id: &str, username: &str, salt: u64) -> String {
    let mut hasher = sha2::Sha256::default();
    hasher.input(person_id.as_bytes());
    hasher.input(username.as_bytes());
    hasher.input(&utils::transform_u64_to_array_of_u8(salt));
    hasher.result().as_slice().to_hex()
}

impl User {
    fn new(person_id: spark::PersonId, username: gerrit::Username) -> User {
        let token = generate_verification_token(&person_id, &username);
        User {
            spark_person_id: person_id,
            gerrit_username: username,
            verified: false,
            verification_token: token,
            enabled: false,
        }
    }
}

/// Describes a state of the bot
#[derive(Debug)]
pub struct Bot {
    users: Vec<User>,
}

enum UserUpdate<'a> {
    NoOp(&'a User),
    Added(&'a User),
    Updated(&'a User),
}

impl Bot {
    pub fn new() -> Bot {
        return Bot { users: Vec::new() };
    }

    /// Return value is the user, and whether the user is a new one.
    fn configure<'a>(&'a mut self,
                     person_id: spark::PersonId,
                     username: gerrit::Username)
                     -> UserUpdate<'a> {
        if let Some(pos) = self.users.iter().position(|ref u| u.spark_person_id == person_id) {
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

    fn enable<'a>(&'a mut self, person_id: &spark::PersonId, enabled: bool) -> Option<&'a User> {
        let pos = self.users.iter().position(|ref u| &u.spark_person_id == person_id);
        match pos {
            Some(pos) => {
                let user: &'a mut User = &mut self.users[pos];
                user.enabled = enabled;
                Some(user)
            }
            None => None,
        }
    }
}

#[derive(Debug)]
pub enum Action {
    Configure(spark::PersonId, gerrit::Username),
    Enable(spark::PersonId),
    Disable(spark::PersonId),
    Help,
    Unknown,
}

const GREETINGS_MSG: &'static str =
    r#"Hi. I am GerritBot. I can watch Gerrit reviews for you, and notify you about new +1/-1's.

For more information, type in **help**.

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

We are almost there. I still need to link your Spark account with your Gerrit account `{}`.
For that, please create a new draft in Gerrit with the folling commit message:

`{}`

After the draft is created, I will get a message from Gerrit, and will notify you here, that
your accounts are linked.
"#;)
}

macro_rules! update_verification_msg {
    () => (r#"Got it!

You have already linked your Spark account previously with a different Gerrit username.
I updated your username to `{}`, but now you have to link your accounts again.

Please create a new draft in Gerrit with the folling commit message:

`{}`
"#;)
}

macro_rules! verification_pending_msg {
    () => (r#"Got it!

A verification for your Gerrit account `{}` is still pending. To complete verification,
please create a new draft in Gerrit with the folling commit message:

`{}`
"#;)
}

const ALREADY_VERIFIED_MSG: &'static str =
    r#"Your Spark account already linked with Gerrit. Nothing to do!"#;

const NOT_CONFIGURED_MSG: &'static str =
    r#"Sorry, your account is not configured. Cf. **configure** and **help**."#;

/// Action controller
/// Return new bot state and an optional message to send to the user
pub fn update(action: Action, bot: Bot) -> (Bot, String) {
    let mut bot = bot;
    let msg = match action {
        Action::Configure(person_id, username) => {
            let user_update = bot.configure(person_id, username);
            match user_update {
                UserUpdate::Added(user) => {
                    format!(verification_msg!(),
                            user.gerrit_username,
                            user.verification_token)
                }
                UserUpdate::Updated(user) => {
                    format!(update_verification_msg!(),
                            user.gerrit_username,
                            user.verification_token)
                }
                UserUpdate::NoOp(user) => {
                    if user.verified {
                        String::from(ALREADY_VERIFIED_MSG)
                    } else {
                        format!(verification_pending_msg!(),
                                user.gerrit_username,
                                user.verification_token)
                    }
                }
            }
        }
        Action::Enable(person_id) => {
            bot.enable(&person_id, true)
                .and_then(|ref user| if user.verified { Some(()) } else { None })
                .map_or(String::from(NOT_CONFIGURED_MSG),
                        |_| String::from("Got it!"))
        }
        Action::Disable(person_id) => {
            bot.enable(&person_id, false)
                .and_then(|ref user| if user.verified { Some(()) } else { None })
                .map_or(String::from(NOT_CONFIGURED_MSG),
                        |_| String::from("Got it!"))
        }
        Action::Help => String::from(HELP_MSG),
        Action::Unknown => String::from(GREETINGS_MSG),
    };
    (bot, msg)
}
