-- Initial grain state schema (PostgreSQL).
-- IF NOT EXISTS keeps this safe for databases created by the pre-migration
-- inline CREATE TABLE path, which lack a _sqlx_migrations tracking row.
CREATE TABLE IF NOT EXISTS grain_state (
    type_name TEXT NOT NULL,
    key       TEXT NOT NULL,
    data      BYTEA NOT NULL,
    version   BIGINT NOT NULL DEFAULT 1,
    PRIMARY KEY (type_name, key)
);
