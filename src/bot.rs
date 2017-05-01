use std::collections::HashSet;

/// Spark id of the user
pub type PersonId = String;

/// Describes a state of the bot
#[derive(Debug)]
pub struct Bot {
    pub persons: HashSet<PersonId>,
}

impl Bot {
    pub fn new() -> Bot {
        return Bot { persons: HashSet::new() };
    }
}

#[derive(Debug)]
pub enum Action {
    Enable(PersonId),
    Disable(PersonId),
    Help,
    Unknown,
}

/// Action controller
pub fn update(action: Action, bot: Bot) -> Bot {
    let mut bot = bot;
    match action {
        Action::Enable(person_id) => {
            bot.persons.insert(person_id);
        }
        Action::Disable(person_id) => {
            bot.persons.remove(&person_id);
        }
        _ => (),
    }
    bot
}
