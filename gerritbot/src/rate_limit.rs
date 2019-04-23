use std::time::Duration;

use lru_time_cache::LruCache;

use gerritbot_gerrit as gerrit;
use gerritbot_spark::PersonId;

use super::state::User;

#[derive(Clone, Default)]
pub struct RateLimiter {
    cache: Option<LruCache<MsgCacheLine, ()>>,
}

impl RateLimiter {
    pub fn with_expiry_duration_and_capacity(expiration: Duration, capacity: usize) -> Self {
        Self {
            cache: Some(LruCache::with_expiry_duration_and_capacity(
                expiration, capacity,
            )),
        }
    }

    pub fn limit<E>(&mut self, user: &User, event: E) -> bool
    where
        E: IntoCacheLine,
    {
        self.cache
            .as_mut()
            .and_then(|cache| {
                cache.insert(
                    IntoCacheLine::into_cache_line(user.spark_person_id.clone(), &event),
                    (),
                )
            })
            .is_some()
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Subject {
    Subject(String),
    Topic(String),
}

impl Subject {
    fn from_change(change: &gerrit::Change) -> Self {
        if let Some(ref topic) = change.topic {
            Subject::Topic(topic.to_string())
        } else {
            Subject::Subject(change.subject.to_string())
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Approval {
    approval_type: String,
    approval_value: String,
}

/// Cache line in LRU Cache containing last approval messages
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MsgCacheLine {
    Approvals {
        person_id: PersonId,
        subject: Subject,
        approver: String,
        approvals: Vec<Approval>,
    },
    ReviewerAdded {
        person_id: PersonId,
        subject: Subject,
    },
}

pub trait IntoCacheLine {
    fn into_cache_line(person_id: PersonId, event: &Self) -> MsgCacheLine;
}

impl IntoCacheLine for &gerrit::CommentAddedEvent {
    fn into_cache_line(person_id: PersonId, event: &Self) -> MsgCacheLine {
        let mut approvals: Vec<_> = event
            .approvals
            .iter()
            .map(
                |gerrit::Approval {
                     ref approval_type,
                     ref value,
                     ..
                 }| Approval {
                    approval_type: approval_type.clone(),
                    approval_value: value.clone(),
                },
            )
            .collect();

        // sort approvals to get a stable key
        approvals.sort_unstable();

        let approver = event
            .author
            .email
            .as_ref()
            .or(event.author.username.as_ref())
            .map(String::as_str)
            .unwrap_or("<unknown user>")
            .to_string();

        MsgCacheLine::Approvals {
            person_id,
            subject: Subject::from_change(&event.change),
            approver,
            approvals,
        }
    }
}

impl IntoCacheLine for &gerrit::ReviewerAddedEvent {
    fn into_cache_line(person_id: PersonId, event: &Self) -> MsgCacheLine {
        MsgCacheLine::ReviewerAdded {
            person_id,
            subject: Subject::from_change(&event.change),
        }
    }
}
