/// Scheduler in-memory store
pub mod store;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::time::Duration;
use url::Url;

/// The representation of a Scheduled job
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ScheduledJob {
    /// The unique ScheduledJob ID which uniquely identifies the job..
    pub id: String,

    /// The HTTP endpoint to POST `ScheduledJob`'s payload when triggered by the CRON schedule.
    pub endpoint: Url,

    /// The CRON expression for the execution schedule.
    pub cron: String,

    /// the raw payload to send to an endpoint.
    pub payload: Box<RawValue>,

    /// When set and enqueuing the `ScheduledJob` fails due to a unique constraint this determines
    /// the backoff + retry period in seconds to try again. No retry if this is not set and will
    /// trigger on its regular cadence.
    #[serde(
        with = "option_duration_u64_serde",
        skip_serializing_if = "Option::is_none"
    )]
    pub retry_already_running: Option<Duration>,

    /// This determines that upon recovery or restart of this scheduler if we should check that the
    /// `ScheduledJob` should have run since the last time it was successfully triggered.
    #[serde(default)]
    pub recovery_check: bool,

    /// This determines the last time the ScheduledJob was successfully triggered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<DateTime<Utc>>,
}

mod option_duration_u64_serde {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if let Some(d) = d {
            s.serialize_u64(d.as_secs())
        } else {
            unreachable!()
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let seconds: Option<u64> = Option::deserialize(deserializer)?;
        match seconds {
            Some(seconds) => Ok(Some(Duration::from_secs(seconds))),
            None => Ok(None),
        }
    }
}
