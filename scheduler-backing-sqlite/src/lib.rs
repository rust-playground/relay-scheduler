use async_stream::stream;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use log::LevelFilter;
use scheduler::store::backing::{Backing, Error, Result};
use scheduler::ScheduledJob;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Error as SQLXError, Pool, Row, Sqlite, SqlitePool};
use std::io::ErrorKind;
use std::pin::Pin;
use std::{str::FromStr, time::Duration};
use tokio_stream::{Stream, StreamExt};
use tracing::error;

/// `SQLite` backing store
pub struct Store {
    pool: Pool<Sqlite>,
}

impl Store {
    /// Creates a new backing store with default settings for `SQLite`.
    ///
    /// # Errors
    ///
    /// Will return `Err` if connecting to the `SQLite` database fails or migrations fail to run.
    #[inline]
    pub async fn default(uri: &str) -> std::result::Result<Self, sqlx::error::Error> {
        let options = SqliteConnectOptions::from_str(uri)?
            .create_if_missing(true)
            .log_statements(LevelFilter::Off)
            .log_slow_statements(LevelFilter::Warn, Duration::from_secs(1))
            .clone();
        Self::new(options).await
    }

    /// Creates a new backing store with advanced options.
    ///
    /// # Errors
    ///
    /// Will return `Err` if connecting to the `SQLite` database fails or migrations fail to run.
    #[inline]
    pub async fn new(
        options: SqliteConnectOptions,
    ) -> std::result::Result<Self, sqlx::error::Error> {
        let pool = SqlitePool::connect_with(options).await?;
        let mut conn = pool.acquire().await?;
        sqlx::migrate!("./migrations").run(&mut conn).await?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl Backing for Store {
    async fn upsert(&self, job: &ScheduledJob) -> Result<()> {
        let blob = serde_json::to_vec(&job).map_err(|e| Error::Upsert {
            job_id: job.id.clone(),
            message: e.to_string(),
            is_retryable: false,
        })?;

        let mut conn = self.pool.acquire().await.map_err(|e| Error::Upsert {
            job_id: job.id.clone(),
            message: e.to_string(),
            is_retryable: is_retryable(e),
        })?;

        sqlx::query("INSERT INTO scheduled_jobs (id, data, last_run) VALUES (?1, ?2, ?3) ON CONFLICT DO UPDATE SET data=excluded.data, last_run=excluded.last_run")
            .bind(&job.id)
            .bind(&blob)
            .bind(&job.last_run)
            .execute(&mut conn)
            .await
            .map_err(|e| Error::Upsert {
                job_id: job.id.clone(),
                message: e.to_string(),
                is_retryable: is_retryable(e),
            })?;

        Ok(())
    }

    async fn touch(&self, job_id: &str, last_run: &DateTime<Utc>) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(|e| Error::Touch {
            job_id: job_id.to_string(),
            message: e.to_string(),
            is_retryable: is_retryable(e),
        })?;

        sqlx::query("UPDATE scheduled_jobs SET last_run=?2 WHERE id=?1")
            .bind(job_id)
            .bind(last_run)
            .execute(&mut conn)
            .await
            .map_err(|e| Error::Touch {
                job_id: job_id.to_string(),
                message: e.to_string(),
                is_retryable: is_retryable(e),
            })?;

        Ok(())
    }

    async fn delete(&self, job_id: &str) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(|e| Error::Delete {
            job_id: job_id.to_string(),
            message: e.to_string(),
            is_retryable: is_retryable(e),
        })?;

        sqlx::query("DELETE FROM scheduled_jobs WHERE id=?1")
            .bind(job_id)
            .execute(&mut conn)
            .await
            .map_err(|e| Error::Delete {
                job_id: job_id.to_string(),
                message: e.to_string(),
                is_retryable: is_retryable(e),
            })?;
        Ok(())
    }

    #[inline]
    fn recover(&'_ self) -> Pin<Box<dyn Stream<Item = Result<ScheduledJob>> + '_>> {
        Box::pin(stream! {
            let mut conn = self.pool.acquire().await.map_err(|e| Error::Recovery {
                message: e.to_string(),
                is_retryable: is_retryable(e),
            })?;

            let mut cursor = sqlx::query("SELECT data FROM scheduled_jobs")
                .fetch(&mut conn);

            while let Some(result) = cursor
                .next()
                .await
            {
                match result {
                    Err(e) => {
                        yield Err(Error::Recovery {
                            message: e.to_string(),
                            is_retryable: is_retryable(e),
                        });
                    }
                    Ok(row) => {
                        let blob: Vec<u8> = row.get(0);
                        let job: ScheduledJob = match serde_json::from_slice(&blob) {
                            Ok(v) => v,
                            Err(e) => {
                                error!("failed to recover data: {}", e.to_string());
                                continue
                            }
                        };
                        yield Ok(job);
                    }
                };
            };
        })
    }
}

#[inline]
fn is_retryable(e: SQLXError) -> bool {
    match e {
        sqlx::Error::Database(ref db) => match db.code() {
            None => false,
            Some(code) => {
                match code.as_ref() {
                    "5" | "10" | "261" | "773" | "513" | "769" | "3338" | "2314" | "266"
                    | "778" => {
                        // 5=SQLITE_BUSY
                        // 10=SQLITE_IOERR
                        // 261=SQLITE_BUSY_RECOVERY
                        // 773=SQLITE_BUSY_TIMEOUT
                        // 513=SQLITE_ERROR_RETRY
                        // 769=SQLITE_ERROR_SNAPSHOT
                        // 3338=SQLITE_IOERR_ACCESS
                        // 2314=SQLITE_IOERR_UNLOCK
                        // 266=SQLITE_IOERR_READ
                        // 778=SQLITE_IOERR_WRITE
                        true
                    }
                    _ => false,
                }
            }
        },
        sqlx::Error::PoolTimedOut => true,
        sqlx::Error::Io(e) => matches!(
            e.kind(),
            ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::NotConnected
                | ErrorKind::WouldBlock
                | ErrorKind::TimedOut
                | ErrorKind::WriteZero
                | ErrorKind::Interrupted
                | ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}
