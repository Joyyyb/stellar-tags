//! Dead-man's switch operator heartbeat renewal (issue #889).
//!
//! The primary admin key is often kept in cold storage. Requiring it for
//! every routine monthly heartbeat exposes the master key unnecessarily.
//! This module lets the admin delegate heartbeat renewal to a designated
//! *operator* address that has **only** the authority to reset the
//! `LastHeartbeat` timestamp — it cannot change the backup admin address,
//! the grace period, or any other contract parameter.
//!
//! # Storage
//! - [`DataKey::HeartbeatOperator`] — `Address` in instance storage. Set by
//!   the admin; cleared when the dead-man's switch is disarmed.
//!
//! # Functions
//! - [`set_heartbeat_operator`] — admin-only; appoints or replaces the operator.
//! - [`renew_heartbeat`] — operator-only; resets `LastHeartbeat` to the
//!   current ledger timestamp without touching any other DMS state.

use crate::errors::Error;
use crate::storage::{DataKey, INSTANCE_BUMP_AMOUNT, INSTANCE_LIFETIME_THRESHOLD};
use soroban_sdk::{Address, Env, Symbol};

/// Appoints `operator` as the address authorised to call [`renew_heartbeat`].
///
/// Only the admin may call this function. Passing the same address as the
/// current admin is rejected to prevent inadvertently collapsing the
/// separation of duties.
///
/// # Parameters
/// - `env` – The Soroban execution environment.
/// - `caller` – Must be the current admin (already authenticated by the caller).
/// - `operator` – Address to designate. Must not equal `caller`.
///
/// # Errors
/// - [`Error::NotInitialized`] — no admin set yet.
/// - [`Error::InvalidDmsConfig`] — `operator == caller` (admin == operator).
pub fn set_heartbeat_operator(env: &Env, caller: &Address, operator: &Address) -> Result<(), Error> {
    // Prevent the admin from appointing itself — it can already call `ping`.
    let admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)?;
    if operator == &admin {
        return Err(Error::InvalidDmsConfig);
    }

    env.storage()
        .instance()
        .set(&DataKey::HeartbeatOperator, operator);
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);

    env.events().publish(
        (Symbol::new(env, "hb_operator_set"), caller.clone()),
        operator.clone(),
    );

    Ok(())
}

/// Resets the dead-man's switch heartbeat clock to the current ledger
/// timestamp.
///
/// The caller must be the designated [`DataKey::HeartbeatOperator`] and must
/// authenticate the invocation. The function is a no-op (returns `Ok`) when
/// the dead-man's switch is not armed (i.e. no [`DataKey::BackupAdmin`] is
/// configured), matching the behaviour of the admin's own `ping` entrypoint.
///
/// The operator **cannot**:
/// - Change the backup admin address.
/// - Modify the `DmsTimeout` grace period.
/// - Access any other contract state.
///
/// # Parameters
/// - `env` – The Soroban execution environment.
/// - `operator` – The calling operator address. Must match
///   [`DataKey::HeartbeatOperator`] and must authorize the call.
///
/// # Errors
/// - [`Error::Unauthorized`] — `operator` is not the designated operator, or
///   no operator has been configured.
pub fn renew_heartbeat(env: &Env, operator: &Address) -> Result<(), Error> {
    // Verify the caller is the designated operator.
    let designated: Address = env
        .storage()
        .instance()
        .get(&DataKey::HeartbeatOperator)
        .ok_or(Error::Unauthorized)?;

    if operator != &designated {
        return Err(Error::Unauthorized);
    }

    // Require the operator to sign this transaction.
    operator.require_auth();

    // Only bump the heartbeat when the DMS is actually armed; mirrors the
    // behaviour of `bump_heartbeat` in lib.rs which is a no-op without a
    // backup admin.
    if !env.storage().instance().has(&DataKey::BackupAdmin) {
        return Ok(());
    }

    env.storage()
        .instance()
        .set(&DataKey::LastHeartbeat, &env.ledger().timestamp());
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);

    env.events().publish(
        (Symbol::new(env, "hb_renewed"), operator.clone()),
        env.ledger().timestamp(),
    );

    Ok(())
}

/// Returns the currently configured heartbeat operator, if any.
pub fn get_heartbeat_operator(env: &Env) -> Option<Address> {
    env.storage().instance().get(&DataKey::HeartbeatOperator)
}
