//! Strongly-consistent cluster membership table — the foundation for the
//! single-activation guarantee (PROD-15, Phase A).
//!
//! This mirrors Orleans' `IMembershipTable`: a durable table with a **single
//! monotonically-increasing version**. Every membership change is an atomic
//! compare-and-swap against the current version, so all silos observe a
//! **totally-ordered sequence of views**. SWIM remains the failure *detector*
//! (deciding whom to probe/suspect), but the authoritative, agreed view — and
//! the view version that the grain directory (Phase B) coordinates on — comes
//! from this table.
//!
//! Orleans reference: `dotnet/orleans` docs `implementation/cluster-management`
//! (the monotonic membership-version row updated via atomic CAS).
//!
//! This module ships the trait, the shared view types, and an in-memory backend
//! for tests/single-process use. Durable backends (SQL via `sqlx`, Redis via
//! `fred`) implement the same trait and are added separately.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;

use crate::hash_ring::SiloAddress;

/// Liveness status of a silo in the membership table.
///
/// Distinct from SWIM's in-memory `MemberStatus` (which only tracks
/// `Alive`/`Suspect` for the detector): the table is the durable record of
/// agreed cluster state, so it also persists `Dead` (the terminal, fenced
/// state) and `Joining` (admitted but not yet serving).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MembershipStatus {
    /// Admitted to the table but still validating connectivity; not yet serving.
    Joining,
    /// Live and serving grains.
    Active,
    /// Suspected dead by at least one silo, not yet declared dead.
    Suspect,
    /// Declared dead. Terminal: a silo that restarts rejoins with a new
    /// `generation`, so references to this incarnation are fenced.
    Dead,
}

/// A single silo's row in the membership table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberEntry {
    pub addr: SiloAddress,
    pub status: MembershipStatus,
    /// The silo's incarnation, set once at startup (start-time ticks in
    /// practice). A restarted silo on the same host:port gets a strictly
    /// higher generation, which is what fences a dead-and-restarted silo.
    pub generation: u64,
    /// Silo ids that have recorded a suspicion of this member.
    pub suspectors: Vec<String>,
    /// Diagnostic-only liveness heartbeat; does **not** affect the view version.
    pub i_am_alive: SystemTime,
}

impl MemberEntry {
    /// Convenience constructor for a freshly-joining member with no suspectors.
    pub fn joining(addr: SiloAddress, generation: u64) -> Self {
        Self {
            addr,
            status: MembershipStatus::Joining,
            generation,
            suspectors: Vec::new(),
            i_am_alive: SystemTime::now(),
        }
    }
}

/// An immutable snapshot of the whole membership table at a given version.
///
/// `version` is monotonically increasing across the cluster: a higher version
/// strictly supersedes a lower one. Two silos that have applied the same set of
/// updates observe the same `version` and the same (sorted) `members`, so they
/// build a byte-identical ring — the property the grain directory relies on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipView {
    pub version: u64,
    /// Members sorted by silo id for deterministic ordering across silos.
    pub members: Vec<MemberEntry>,
}

impl MembershipView {
    /// The addresses of members currently `Active` (eligible to host grains and
    /// own directory partitions), in deterministic order.
    pub fn active_silos(&self) -> Vec<SiloAddress> {
        self.members
            .iter()
            .filter(|m| m.status == MembershipStatus::Active)
            .map(|m| m.addr.clone())
            .collect()
    }

    /// Look up a member's row by silo id.
    pub fn member(&self, silo_id: &str) -> Option<&MemberEntry> {
        self.members.iter().find(|m| m.addr.silo_id == silo_id)
    }
}

/// Errors returned by a [`MembershipTable`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MembershipError {
    /// The caller's `expected_version` was stale. The caller must re-read the
    /// table and retry its read-modify-write (Orleans' optimistic CAS loop).
    #[error("membership version conflict: expected {expected}, table at {actual}")]
    VersionConflict { expected: u64, actual: u64 },
    /// Operation referenced a silo not present in the table.
    #[error("member not found: {0}")]
    NotFound(String),
    /// The underlying durable store failed.
    #[error("membership backend error: {0}")]
    Backend(String),
}

/// A strongly-consistent membership table.
///
/// Writes are serialized through a single monotonic version via
/// [`try_update`](MembershipTable::try_update)'s compare-and-swap, giving a
/// total order over membership changes.
#[async_trait]
pub trait MembershipTable: Send + Sync + 'static {
    /// Read the current view (version + all members).
    async fn read_all(&self) -> Result<MembershipView, MembershipError>;

    /// Atomically insert-or-replace `entry` (keyed by silo id) and increment the
    /// table version, but only if `expected_version` equals the current version.
    /// On success returns the new view (with the bumped version). On a stale
    /// `expected_version` returns [`MembershipError::VersionConflict`] so the
    /// caller can re-read and retry.
    async fn try_update(
        &self,
        entry: MemberEntry,
        expected_version: u64,
    ) -> Result<MembershipView, MembershipError>;

    /// Update a member's diagnostic liveness timestamp. This does **not** change
    /// the view version (it is not part of the agreed view), matching Orleans'
    /// `IAmAlive` semantics.
    async fn update_i_am_alive(&self, silo_id: &str) -> Result<(), MembershipError>;
}

/// In-memory [`MembershipTable`] for tests and single-process use.
///
/// Linearizable by construction: all operations take a single `Mutex`, so the
/// CAS on `version` is atomic with the member write.
#[derive(Debug, Default)]
pub struct InMemoryMembershipTable {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    version: u64,
    /// silo_id -> entry
    members: HashMap<String, MemberEntry>,
}

impl InMemoryMembershipTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a deterministic, version-stamped view from the current members.
    fn snapshot(inner: &Inner) -> MembershipView {
        let mut members: Vec<MemberEntry> = inner.members.values().cloned().collect();
        members.sort_by(|a, b| a.addr.silo_id.cmp(&b.addr.silo_id));
        MembershipView {
            version: inner.version,
            members,
        }
    }
}

#[async_trait]
impl MembershipTable for InMemoryMembershipTable {
    async fn read_all(&self) -> Result<MembershipView, MembershipError> {
        let inner = self.inner.lock().expect("membership table mutex poisoned");
        Ok(Self::snapshot(&inner))
    }

    async fn try_update(
        &self,
        entry: MemberEntry,
        expected_version: u64,
    ) -> Result<MembershipView, MembershipError> {
        let mut inner = self.inner.lock().expect("membership table mutex poisoned");
        if inner.version != expected_version {
            return Err(MembershipError::VersionConflict {
                expected: expected_version,
                actual: inner.version,
            });
        }
        inner.members.insert(entry.addr.silo_id.clone(), entry);
        inner.version += 1;
        Ok(Self::snapshot(&inner))
    }

    async fn update_i_am_alive(&self, silo_id: &str) -> Result<(), MembershipError> {
        let mut inner = self.inner.lock().expect("membership table mutex poisoned");
        match inner.members.get_mut(silo_id) {
            // Diagnostic-only: deliberately does not bump `version`.
            Some(entry) => {
                entry.i_am_alive = SystemTime::now();
                Ok(())
            }
            None => Err(MembershipError::NotFound(silo_id.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(id: &str) -> SiloAddress {
        SiloAddress {
            host: "127.0.0.1".into(),
            port: 7000,
            silo_id: id.into(),
        }
    }

    fn active(id: &str, generation: u64) -> MemberEntry {
        MemberEntry {
            addr: addr(id),
            status: MembershipStatus::Active,
            generation,
            suspectors: Vec::new(),
            i_am_alive: SystemTime::now(),
        }
    }

    #[tokio::test]
    async fn empty_table_starts_at_version_zero() {
        let table = InMemoryMembershipTable::new();
        let view = table.read_all().await.unwrap();
        assert_eq!(view.version, 0);
        assert!(view.members.is_empty());
    }

    #[tokio::test]
    async fn try_update_inserts_and_bumps_version() {
        let table = InMemoryMembershipTable::new();
        let view = table.try_update(active("silo-a", 1), 0).await.unwrap();
        assert_eq!(view.version, 1);
        assert_eq!(view.members.len(), 1);
        assert_eq!(view.member("silo-a").unwrap().generation, 1);
    }

    #[tokio::test]
    async fn stale_expected_version_conflicts() {
        let table = InMemoryMembershipTable::new();
        table.try_update(active("silo-a", 1), 0).await.unwrap(); // -> version 1
        // A second writer that still believes the version is 0 must be rejected.
        let err = table.try_update(active("silo-b", 1), 0).await.unwrap_err();
        assert_eq!(err, MembershipError::VersionConflict { expected: 0, actual: 1 });
    }

    #[tokio::test]
    async fn conflicting_writer_succeeds_after_reread() {
        let table = InMemoryMembershipTable::new();
        table.try_update(active("silo-a", 1), 0).await.unwrap(); // version 1

        // Stale write fails...
        assert!(table.try_update(active("silo-b", 1), 0).await.is_err());
        // ...re-read the current version and retry (Orleans' CAS loop).
        let current = table.read_all().await.unwrap().version;
        let view = table.try_update(active("silo-b", 1), current).await.unwrap();
        assert_eq!(view.version, 2);
        assert_eq!(view.members.len(), 2);
    }

    #[tokio::test]
    async fn version_is_strictly_monotonic_and_replaces_in_place() {
        let table = InMemoryMembershipTable::new();
        let mut v = 0;
        for g in 1..=3 {
            v = table.try_update(active("silo-a", g), v).await.unwrap().version;
        }
        // Three updates to the same silo id => version 3, still one member,
        // last write wins (generation 3, status Active).
        assert_eq!(v, 3);
        let view = table.read_all().await.unwrap();
        assert_eq!(view.members.len(), 1);
        assert_eq!(view.member("silo-a").unwrap().generation, 3);
    }

    #[tokio::test]
    async fn two_tables_fed_same_updates_agree() {
        // The core invariant: identical update sequences => identical views, so
        // any ring built from the view is byte-identical across silos.
        let (t1, t2) = (InMemoryMembershipTable::new(), InMemoryMembershipTable::new());
        for (id, g) in [("silo-c", 3u64), ("silo-a", 1), ("silo-b", 2)] {
            // Same logical update applied to both tables: build the entry once
            // (one timestamp) and replicate it, as a real agreed view would.
            let entry = active(id, g);
            let v1 = t1.read_all().await.unwrap().version;
            t1.try_update(entry.clone(), v1).await.unwrap();
            let v2 = t2.read_all().await.unwrap().version;
            t2.try_update(entry, v2).await.unwrap();
        }
        let view1 = t1.read_all().await.unwrap();
        let view2 = t2.read_all().await.unwrap();
        assert_eq!(view1, view2);
        // Deterministic sort order regardless of insertion order.
        let ids: Vec<_> = view1.members.iter().map(|m| m.addr.silo_id.as_str()).collect();
        assert_eq!(ids, ["silo-a", "silo-b", "silo-c"]);
        assert_eq!(view1.active_silos().len(), 3);
    }

    #[tokio::test]
    async fn i_am_alive_does_not_bump_version() {
        let table = InMemoryMembershipTable::new();
        table.try_update(active("silo-a", 1), 0).await.unwrap(); // version 1
        table.update_i_am_alive("silo-a").await.unwrap();
        assert_eq!(table.read_all().await.unwrap().version, 1, "I-am-alive is diagnostic only");

        let err = table.update_i_am_alive("ghost").await.unwrap_err();
        assert_eq!(err, MembershipError::NotFound("ghost".into()));
    }

    #[tokio::test]
    async fn status_transition_to_dead_is_recorded() {
        let table = InMemoryMembershipTable::new();
        let mut v = table.try_update(active("silo-a", 1), 0).await.unwrap().version;
        let mut dead = active("silo-a", 1);
        dead.status = MembershipStatus::Dead;
        v = table.try_update(dead, v).await.unwrap().version;
        assert_eq!(v, 2);
        let view = table.read_all().await.unwrap();
        assert_eq!(view.member("silo-a").unwrap().status, MembershipStatus::Dead);
        assert!(view.active_silos().is_empty(), "dead silo is not active");
    }
}
