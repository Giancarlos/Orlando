//! Transactional grain example (automatic rollback on failure).
//!
//! A transactional grain snapshots its state before each handler. If the
//! handler returns `Err`, the mailbox reverts to the snapshot — so a handler can
//! mutate state optimistically and bail out, leaving no partial change behind.
//! (State must be `Clone` for snapshotting.)
//!
//! Run with: `cargo run -p orlando-persistence --example transactional_grain`

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainContext, Message};
use orlando_persistence::{
    InMemoryStateStore, PersistenceError, PersistentGrain, PersistentSilo, TransactionContext,
    TransactionalGrain, TransactionalHandler,
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

// ── Messages ────────────────────────────────────────────────────

struct Deposit(i64);
impl Message for Deposit {
    type Result = i64;
}
struct Withdraw(i64);
impl Message for Withdraw {
    type Result = i64;
}
struct GetBalance;
impl Message for GetBalance {
    type Result = i64;
}

// ── Handlers (return Result; an Err triggers rollback) ──────────

#[async_trait]
impl TransactionalHandler<Deposit> for Account {
    async fn handle(state: &mut AccountState, msg: Deposit, _c: &GrainContext, _tx: &TransactionContext) -> Result<i64, PersistenceError> {
        state.balance += msg.0;
        Ok(state.balance)
    }
}

#[async_trait]
impl TransactionalHandler<Withdraw> for Account {
    async fn handle(state: &mut AccountState, msg: Withdraw, _c: &GrainContext, _tx: &TransactionContext) -> Result<i64, PersistenceError> {
        // Mutate optimistically...
        state.balance -= msg.0;
        // ...then bail if it overdrew. Returning Err rolls back the mutation
        // above. (A real app would define its own error type; we reuse a
        // PersistenceError variant just to signal failure here.)
        if state.balance < 0 {
            return Err(PersistenceError::Io(std::io::Error::other("insufficient funds")));
        }
        Ok(state.balance)
    }
}

#[async_trait]
impl TransactionalHandler<GetBalance> for Account {
    async fn handle(state: &mut AccountState, _m: GetBalance, _c: &GrainContext, _tx: &TransactionContext) -> Result<i64, PersistenceError> {
        Ok(state.balance)
    }
}

#[tokio::main]
async fn main() {
    let silo = PersistentSilo::builder().store(InMemoryStateStore::new()).build();
    let acct = silo.transactional_get_ref::<Account>("acct-1");

    println!("deposit 100 -> {}", acct.ask(Deposit(100)).await.unwrap());
    println!("withdraw 30 -> {}", acct.ask(Withdraw(30)).await.unwrap());

    // This withdraw overdraws: the handler decrements then returns Err, so the
    // decrement is rolled back.
    match acct.ask(Withdraw(1000)).await {
        Ok(b) => println!("withdraw 1000 -> {b}"),
        Err(e) => println!("withdraw 1000 rejected ({e}) — state rolled back"),
    }

    let balance = acct.ask(GetBalance).await.unwrap();
    println!("final balance = {balance}");
    assert_eq!(balance, 70, "failed withdraw must not have changed the balance");
    println!("balance intact after the failed transaction ✓");
}
