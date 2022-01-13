CREATE TABLE IF NOT EXISTS scheduled_jobs (
    id varchar NOT NULL,
    data blob NOT NULL,
    last_run TIMESTAMP DEFAULT NULL,
    PRIMARY KEY (id)
);