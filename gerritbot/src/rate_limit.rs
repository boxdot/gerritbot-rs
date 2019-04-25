use std::time::Duration;

use lru_time_cache::LruCache;

use gerritbot_gerrit as gerrit;

#[derive(Clone, Default)]
pub struct RateLimiter {
    cache: Option<LruCache<MsgCacheLine, ()>>,
}

impl RateLimiter {
    pub fn with_expiry_duration_and_capacity(expiration: Duration, capacity: usize) -> Self {
        Self {
            cache: Some(LruCache::with_expiry_duration_and_capacity(expiration, capacity)),
        }
    }

    pub fn limit<E>(&mut self, user_index: usize, subject: &str, event: E) -> bool
    where
        E: IntoCacheLine,
    {
        self.cache
            .as_mut()
            .and_then(|cache| {
                cache.insert(
                    IntoCacheLine::into_cache_line(user_index, subject, &event),
                    (),
                )
            })
            .is_some()
    }
}

/// Cache line in LRU Cache containing last approval messages
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MsgCacheLine {
    Approval {
        /// position of the user in bots.user vector
        user_ref: usize,
        subject: String,
        approver: String,
        approval_type: String,
        approval_value: String,
    },
    ReviewerAdded {
        user_ref: usize,
        subject: String,
    },
}

pub trait IntoCacheLine {
    fn into_cache_line(user_index: usize, subject: &str, event: &Self) -> MsgCacheLine;
}

impl IntoCacheLine for (&String, &gerrit::Approval) {
    fn into_cache_line(user_index: usize, subject: &str, event: &Self) -> MsgCacheLine {
        MsgCacheLine::Approval {
            user_ref: user_index,
            subject: subject.to_string(),
            approver: event.0.clone(),
            approval_type: event.1.approval_type.clone(),
            approval_value: event.1.value.clone(),
        }
    }
}

impl IntoCacheLine for &gerrit::ReviewerAddedEvent {
    fn into_cache_line(user_index: usize, subject: &str, _event: &Self) -> MsgCacheLine {
        MsgCacheLine::ReviewerAdded {
            user_ref: user_index,
            subject: subject.to_string(),
        }
    }
}
