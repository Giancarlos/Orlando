//! PROD-24 audit: pin the documented behavior of `TransactionCoordinator` —
//! it is a saga coordinator, NOT 2PC, and provides no cross-grain atomicity.
//!
//! This test deliberately asserts the *limitation*: when a later step in a
//! transaction fails, earlier grains' committed changes remain (they are not
//! rolled back across grains). If this ever changes to true atomicity, this
//! test should be updated — it documents intent, not a bug.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, Message};
use orlando_persistence::{
    InMemoryStateStore, PersistenceError, PersistentGrain, PersistentSilo, TransactionContext,
    TransactionCoordinator, TransactionError, TransactionalGrain, TransactionalHandler,
};

#[derive(Default, Clone, Serialize, Deserialize)]
struct AccountState {
    balance: i64,
}

struct Account;

#[async_trait]
impl Grain for Account {
    type State = AccountState;
}
impl PersistentGrain for Account {}
impl TransactionalGrain for Account {}

struct Debit(i64);
impl Message for Debit {
    type Result = i64;
}
struct Fail;
impl Message for Fail {
    type Result = ();
}
struct GetBalance;
impl Message for GetBalance {
    type Result = i64;
}

#[async_trait]
impl TransactionalHandler<Debit> for Account {
    async fn handle(s: &mut AccountState, m: Debit, _c: &GrainContext, _t: &TransactionContext) -> Result<i64, PersistenceError> {
        s.balance -= m.0;
        Ok(s.balance)
    }
}

#[async_trait]
impl TransactionalHandler<Fail> for Account {
    async fn handle(_s: &mut AccountState, _m: Fail, _c: &GrainContext, _t: &TransactionContext) -> Result<(), PersistenceError> {
        Err(PersistenceError::Io(std::io::Error::other("participant refused")))
    }
}

#[async_trait]
impl TransactionalHandler<GetBalance> for Account {
    async fn handle(s: &mut AccountState, _m: GetBalance, _c: &GrainContext, _t: &TransactionContext) -> Result<i64, PersistenceError> {
        Ok(s.balance)
    }
}

#[tokio::test]
async fn coordinator_is_saga_not_atomic_across_grains() {
    let silo = PersistentSilo::builder().store(InMemoryStateStore::new()).build();
    let account_a = silo.transactional_get_ref::<Account>("a");
    let account_b = silo.transactional_get_ref::<Account>("b");

    let tx = TransactionCoordinator::new();
    let result = tx
        .execute(|_tx_id| async {
            // Step 1 succeeds and COMMITS in account A...
            account_a.ask(Debit(50)).await?;
            // ...step 2 fails on account B.
            account_b.ask(Fail).await?;
            Ok(())
        })
        .await;

    // The transaction reports aborted...
    assert!(
        matches!(result, Err(TransactionError::Aborted { .. })),
        "a failed step must abort the transaction"
    );

    // ...but account A's debit is NOT rolled back (no cross-grain atomicity).
    // This is the saga semantics the coordinator documents: compensate at the
    // application layer.
    let balance_a = account_a.ask(GetBalance).await.unwrap();
    assert_eq!(balance_a, -50, "account A's committed debit survives the abort (saga, not 2PC)");

    // account B rolled back its own (failed) handler, so it is untouched.
    let balance_b = account_b.ask(GetBalance).await.unwrap();
    assert_eq!(balance_b, 0, "account B's failed handler rolled back its own state");
}
