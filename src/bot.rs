/// Spark id of the user
pub type PersonId = String;

/// Describes a state of the bot
pub struct Bot {
    pub persons: Vec<PersonId>,
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
    bot
}
