use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, Message};
use orlando_persistence::{
    InMemoryStateStore, PersistenceError, PersistentGrain, PersistentSilo,
    TransactionCoordinator, TransactionError, TransactionalGrain, TransactionalHandler,
};

// --- State ---

#[derive(Default, Clone, Serialize, Deserialize)]
struct AccountState {
    balance: i64,
}

// --- Grains ---

struct AccountA;
struct AccountB;

#[async_trait]
impl Grain for AccountA {
    type State = AccountState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(500)
    }
}

impl PersistentGrain for AccountA {}
impl TransactionalGrain for AccountA {}

#[async_trait]
impl Grain for AccountB {
    type State = AccountState;

    fn idle_timeout() -> Duration {
        Duration::from_millis(500)
    }
}

impl PersistentGrain for AccountB {}
impl TransactionalGrain for AccountB {}

// --- Messages ---

struct Credit {
    amount: i64,
}
impl Message for Credit {
    type Result = i64;
}

struct Debit {
    amount: i64,
}
impl Message for Debit {
    type Result = i64;
}

/// A debit that fails if insufficient funds.
struct DebitChecked {
    amount: i64,
}
impl Message for DebitChecked {
    type Result = i64;
}

struct GetBalance;
impl Message for GetBalance {
    type Result = i64;
}

// --- Handlers for AccountA ---

#[async_trait]
impl TransactionalHandler<Credit> for AccountA {
    async fn handle(
        state: &mut AccountState,
        msg: Credit,
        _ctx: &GrainContext,
        _tx: &orlando_persistence::TransactionContext,
    ) -> Result<i64, PersistenceError> {
        state.balance += msg.amount;
        Ok(state.balance)
    }
}

#[async_trait]
impl TransactionalHandler<Debit> for AccountA {
    async fn handle(
        state: &mut AccountState,
        msg: Debit,
        _ctx: &GrainContext,
        _tx: &orlando_persistence::TransactionContext,
    ) -> Result<i64, PersistenceError> {
        state.balance -= msg.amount;
        Ok(state.balance)
    }
}

#[async_trait]
impl TransactionalHandler<DebitChecked> for AccountA {
    async fn handle(
        state: &mut AccountState,
        msg: DebitChecked,
        _ctx: &GrainContext,
        _tx: &orlando_persistence::TransactionContext,
    ) -> Result<i64, PersistenceError> {
        if state.balance < msg.amount {
            return Err(PersistenceError::Serialization(
                "insufficient funds".to_string(),
            ));
        }
        state.balance -= msg.amount;
        Ok(state.balance)
    }
}

#[async_trait]
impl orlando_core::GrainHandler<GetBalance> for AccountA {
    async fn handle(state: &mut AccountState, _msg: GetBalance, _ctx: &GrainContext) -> i64 {
        state.balance
    }
}

// --- Handlers for AccountB ---

#[async_trait]
impl TransactionalHandler<Credit> for AccountB {
    async fn handle(
        state: &mut AccountState,
        msg: Credit,
        _ctx: &GrainContext,
        _tx: &orlando_persistence::TransactionContext,
    ) -> Result<i64, PersistenceError> {
        state.balance += msg.amount;
        Ok(state.balance)
    }
}

#[async_trait]
impl TransactionalHandler<Debit> for AccountB {
    async fn handle(
        state: &mut AccountState,
        msg: Debit,
        _ctx: &GrainContext,
        _tx: &orlando_persistence::TransactionContext,
    ) -> Result<i64, PersistenceError> {
        state.balance -= msg.amount;
        Ok(state.balance)
    }
}

#[async_trait]
impl TransactionalHandler<DebitChecked> for AccountB {
    async fn handle(
        state: &mut AccountState,
        msg: DebitChecked,
        _ctx: &GrainContext,
        _tx: &orlando_persistence::TransactionContext,
    ) -> Result<i64, PersistenceError> {
        if state.balance < msg.amount {
            return Err(PersistenceError::Serialization(
                "insufficient funds".to_string(),
            ));
        }
        state.balance -= msg.amount;
        Ok(state.balance)
    }
}

#[async_trait]
impl orlando_core::GrainHandler<GetBalance> for AccountB {
    async fn handle(state: &mut AccountState, _msg: GetBalance, _ctx: &GrainContext) -> i64 {
        state.balance
    }
}

// --- Tests ---

/// Transfer between two accounts succeeds — both balances update.
#[tokio::test]
async fn multi_grain_transfer_succeeds() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let acct_a = silo.transactional_get_ref::<AccountA>("a1");
    let acct_b = silo.transactional_get_ref::<AccountB>("b1");

    // Seed account A with 1000
    acct_a.ask(Credit { amount: 1000 }).await.unwrap();

    // Transfer 300 from A to B
    let tx = TransactionCoordinator::new();
    let acct_a_clone = acct_a.clone();
    let acct_b_clone = acct_b.clone();

    let result = tx
        .execute(|_tx_id| async move {
            acct_a_clone.ask(Debit { amount: 300 }).await?;
            acct_b_clone.ask(Credit { amount: 300 }).await?;
            Ok(())
        })
        .await;

    assert!(result.is_ok(), "transfer should succeed");

    // Verify balances through persistent refs (non-transactional read)
    let reader_a = silo.persistent_get_ref::<AccountA>("a1");
    let reader_b = silo.persistent_get_ref::<AccountB>("b1");

    let balance_a = reader_a.ask(GetBalance).await.unwrap();
    let balance_b = reader_b.ask(GetBalance).await.unwrap();

    assert_eq!(balance_a, 700, "A should have 1000 - 300 = 700");
    assert_eq!(balance_b, 300, "B should have 0 + 300 = 300");
}

/// When the second grain's operation fails, the coordinator returns an error.
/// The second grain rolls back automatically (TransactionalGrainRef).
/// The first grain's debit already committed in-grain.
#[tokio::test]
async fn multi_grain_second_fails_returns_aborted() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let acct_a = silo.transactional_get_ref::<AccountA>("a2");
    let acct_b = silo.transactional_get_ref::<AccountB>("b2");

    // Seed A with 1000, B with 0
    acct_a.ask(Credit { amount: 1000 }).await.unwrap();

    // Try to transfer 500 from A, but B's checked debit will fail (B has 0)
    let tx = TransactionCoordinator::new();
    let acct_a_clone = acct_a.clone();
    let acct_b_clone = acct_b.clone();

    let result = tx
        .execute(|_tx_id| async move {
            acct_a_clone.ask(Debit { amount: 500 }).await?;
            // B has 0 balance — this checked debit should fail
            acct_b_clone
                .ask(DebitChecked { amount: 500 })
                .await?;
            Ok(())
        })
        .await;

    assert!(
        matches!(result, Err(TransactionError::Aborted { .. })),
        "transaction should be aborted when B fails"
    );

    // B's state rolled back (was 0, DebitChecked failed -> still 0)
    let reader_b = silo.persistent_get_ref::<AccountB>("b2");
    let balance_b = reader_b.ask(GetBalance).await.unwrap();
    assert_eq!(balance_b, 0, "B should remain at 0 after rollback");
}

/// Transaction times out if a grain call takes too long.
#[tokio::test]
async fn multi_grain_timeout() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let acct_a = silo.transactional_get_ref::<AccountA>("a3");

    // Seed A
    acct_a.ask(Credit { amount: 100 }).await.unwrap();

    let tx = TransactionCoordinator::new().with_timeout(Duration::from_millis(50));

    let result = tx
        .execute(|_tx_id| async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok::<(), orlando_core::GrainError>(())
        })
        .await;

    assert!(
        matches!(result, Err(TransactionError::Timeout(_, _))),
        "should timeout"
    );
}

/// Multiple sequential transactions on the same grains work correctly.
#[tokio::test]
async fn sequential_multi_grain_transactions() {
    let store = InMemoryStateStore::new();
    let silo = PersistentSilo::builder().store(store).build();

    let acct_a = silo.transactional_get_ref::<AccountA>("a4");
    let acct_b = silo.transactional_get_ref::<AccountB>("b4");

    // Seed both with 500
    acct_a.ask(Credit { amount: 500 }).await.unwrap();
    acct_b.ask(Credit { amount: 500 }).await.unwrap();

    // Transfer 100 from A to B three times
    for _ in 0..3 {
        let tx = TransactionCoordinator::new();
        let a = acct_a.clone();
        let b = acct_b.clone();
        tx.execute(|_tx_id| async move {
            a.ask(Debit { amount: 100 }).await?;
            b.ask(Credit { amount: 100 }).await?;
            Ok(())
        })
        .await
        .unwrap();
    }

    let reader_a = silo.persistent_get_ref::<AccountA>("a4");
    let reader_b = silo.persistent_get_ref::<AccountB>("b4");

    let balance_a = reader_a.ask(GetBalance).await.unwrap();
    let balance_b = reader_b.ask(GetBalance).await.unwrap();

    assert_eq!(balance_a, 200, "A: 500 - 3*100 = 200");
    assert_eq!(balance_b, 800, "B: 500 + 3*100 = 800");
}

/// The tx_id is accessible and unique per coordinator.
#[tokio::test]
async fn tx_id_is_passed_to_closure() {
    let tx = TransactionCoordinator::new();
    let expected_id = tx.tx_id().clone();

    let received_id = tx
        .execute(|tx_id| async move { Ok::<_, orlando_core::GrainError>(tx_id) })
        .await
        .unwrap();

    assert_eq!(received_id, expected_id);
}
