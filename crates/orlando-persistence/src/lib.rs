pub(crate) mod backoff;
pub(crate) mod facet;
mod file_store;
mod journal_store;
mod journaled_grain;
pub(crate) mod journaled_mailbox;
mod memory_journal_store;
mod memory_store;
mod multi_transaction;
mod persistent_grain;
pub(crate) mod persistent_mailbox;
mod persistent_silo;
mod replication_log;
pub(crate) mod replication_sink;
mod serializer;
mod sqlite_replication_log;
mod sqlite_store;
mod store;

#[cfg(feature = "postgres")]
mod postgres_replication_log;
#[cfg(feature = "postgres")]
mod postgres_store;
mod transaction;
mod versioned_grain;

pub use file_store::FileStateStore;
pub use journal_store::{JournalEntry, JournalStore};
pub use journaled_grain::{JournaledGrain, JournaledHandler};
pub use memory_journal_store::InMemoryJournalStore;
pub use memory_store::InMemoryStateStore;
pub use multi_transaction::{TransactionCoordinator, TransactionError, TxId};
pub use facet::{FacetContext, FacetDescriptor};
pub use persistent_grain::{FacetedHandler, PersistentGrain, TransactionalGrain, TransactionalHandler};
pub use persistent_silo::{
    FacetedGrainRef, JournaledGrainRef, PersistentSilo, PersistentSiloBuilder, TransactionalGrainRef,
};
pub use sqlite_replication_log::SqliteReplicationLog;
pub use sqlite_store::SqliteStateStore;
#[cfg(feature = "postgres")]
pub use postgres_replication_log::PostgresReplicationLog;
#[cfg(feature = "postgres")]
pub use postgres_store::PostgresStateStore;
pub use replication_log::{InMemoryReplicationLog, ReplicationError, ReplicationLog};
pub use replication_sink::ReplicationSink;
pub use serializer::SerializerFormat;
pub use store::{ETag, PersistenceError, PersistenceStrategy, StateStore};
pub use transaction::TransactionContext;
pub use versioned_grain::{VersionedGrain, migrate_state};

#[cfg(feature = "redis")]
mod redis_store;

#[cfg(feature = "redis")]
pub use redis_store::RedisStateStore;
