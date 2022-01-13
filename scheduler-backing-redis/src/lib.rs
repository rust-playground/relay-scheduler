use async_stream::stream;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redis::{Client, ErrorKind, RedisError};
use scheduler::store::backing::{Backing, Error, Result};
use scheduler::ScheduledJob;
use std::pin::Pin;
use tokio_stream::Stream;

/// Redis backing store
pub struct Store {
    client: Client,
}

impl Store {
    /// Creates a new backing store with default settings for Redis.
    ///
    /// # Errors
    ///
    /// Will return `Err` if connecting to the server fails.
    pub async fn default(uri: &str) -> std::result::Result<Self, RedisError> {
        let client = Client::open(uri)?;
        let mut conn = client.get_async_connection().await?;
        redis::cmd("PING").query_async(&mut conn).await?;
        Ok(Self { client })
    }
}

#[async_trait]
impl Backing for Store {
    async fn upsert(&self, job: &ScheduledJob) -> Result<()> {
        let mut conn = self
            .client
            .get_async_connection()
            .await
            .map_err(|e| Error::Upsert {
                job_id: job.id.clone(),
                message: e.to_string(),
                is_retryable: is_retryable(&e),
            })?;

        let data: String = serde_json::to_string(&job).map_err(|e| Error::Upsert {
            job_id: job.id.clone(),
            message: e.to_string(),
            is_retryable: false,
        })?;

        redis::cmd("SET")
            .arg(&job.id)
            .arg(&data)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Upsert {
                job_id: job.id.clone(),
                message: e.to_string(),
                is_retryable: is_retryable(&e),
            })?;

        Ok(())
    }

    async fn touch(&self, job_id: &str, last_run: &DateTime<Utc>) -> Result<()> {
        let mut conn = self
            .client
            .get_async_connection()
            .await
            .map_err(|e| Error::Touch {
                job_id: job_id.to_string(),
                message: e.to_string(),
                is_retryable: is_retryable(&e),
            })?;

        let s: String = redis::cmd("GET")
            .arg(&job_id)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Touch {
                job_id: job_id.to_string(),
                message: e.to_string(),
                is_retryable: is_retryable(&e),
            })?;
        let mut job: ScheduledJob = serde_json::from_str(&s).map_err(|e| Error::Touch {
            job_id: job_id.to_string(),
            message: e.to_string(),
            is_retryable: false,
        })?;
        job.last_run = Some(*last_run);

        let data: String = serde_json::to_string(&job).map_err(|e| Error::Touch {
            job_id: job.id.clone(),
            message: e.to_string(),
            is_retryable: false,
        })?;

        redis::cmd("SET")
            .arg(&job.id)
            .arg(&data)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Touch {
                job_id: job.id.clone(),
                message: e.to_string(),
                is_retryable: is_retryable(&e),
            })?;

        Ok(())
    }

    async fn delete(&self, job_id: &str) -> Result<()> {
        let mut conn = self
            .client
            .get_async_connection()
            .await
            .map_err(|e| Error::Delete {
                job_id: job_id.to_string(),
                message: e.to_string(),
                is_retryable: is_retryable(&e),
            })?;

        redis::cmd("DEL")
            .arg(&job_id)
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Delete {
                job_id: job_id.to_string(),
                message: e.to_string(),
                is_retryable: is_retryable(&e),
            })?;

        Ok(())
    }

    #[inline]
    fn recover(&'_ self) -> Pin<Box<dyn Stream<Item = Result<ScheduledJob>> + '_>> {
        Box::pin(stream! {

            let mut conn = self
                .client
                .get_async_connection()
                .await
                .map_err(|e| Error::Recovery {
                    message: e.to_string(),
                    is_retryable: is_retryable(&e),
                })?;

            let ids: Vec<String> =
                    redis::cmd("KEYS")
                        .arg("*")
                        .query_async(&mut conn)
                        .await
                        .map_err(|e| Error::Recovery {
                            message: e.to_string(),
                            is_retryable: is_retryable(&e),
                        })?;

            for id in ids {
                let s: String = redis::cmd("GET")
                    .arg(&id)
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| Error::Recovery {
                        message: e.to_string(),
                        is_retryable: is_retryable(&e),
                    })?;
                let job: ScheduledJob = serde_json::from_str(&s).map_err(|e| Error::Recovery {
                    message: e.to_string(),
                    is_retryable: false,
                })?;
                yield Ok(job);
            }
        })
    }
}

#[inline]
fn is_retryable(e: &RedisError) -> bool {
    match e.kind() {
        ErrorKind::BusyLoadingError | ErrorKind::TryAgain => true,
        ErrorKind::IoError => e.is_timeout() || e.is_connection_dropped(),
        _ => false,
    }
}
