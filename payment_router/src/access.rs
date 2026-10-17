use crate::errors::Error;
use crate::storage::{
    DataKey, Role, INSTANCE_BUMP_AMOUNT, INSTANCE_LIFETIME_THRESHOLD, PERSISTENT_BUMP_AMOUNT,
    PERSISTENT_LIFETIME_THRESHOLD,
};
use crate::types::ActionType;
use soroban_sdk::{Address, BytesN, Env, Symbol, Vec};

// ── Role-Based Access Control (RBAC) Helpers ─────────────────────────────────

pub fn set_role_internal(env: &Env, role: Role, account: &Address) {
    if let Some(previous) = env
        .storage()
        .instance()
        .get::<DataKey, Address>(&DataKey::Role(role))
    {
        if &previous != account {
            env.storage()
                .persistent()
                .set(&DataKey::UserRole(previous, role), &false);
        }
    }
    env.storage().instance().set(&DataKey::Role(role), account);
    env.storage()
        .persistent()
        .set(&DataKey::UserRole(account.clone(), role), &true);
    env.storage().persistent().extend_ttl(
        &DataKey::UserRole(account.clone(), role),
        PERSISTENT_LIFETIME_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );
}

pub fn remove_role_internal(env: &Env, role: Role, account: &Address) {
    if let Some(current) = env
        .storage()
        .instance()
        .get::<DataKey, Address>(&DataKey::Role(role))
    {
        if current == *account {
            env.storage().instance().remove(&DataKey::Role(role));
        }
    }
    env.storage()
        .persistent()
        .set(&DataKey::UserRole(account.clone(), role), &false);
}

pub fn apply_role_grant(env: &Env, grantee: &Address, role: Role) {
    set_role_internal(env, role, grantee);
    env.events().publish(
        (Symbol::new(env, "role_assigned"), role, grantee.clone()),
        env.ledger().timestamp(),
    );
}

pub fn apply_role_revoke(env: &Env, grantee: &Address, role: Role) {
    remove_role_internal(env, role, grantee);
    env.events().publish(
        (Symbol::new(env, "role_revoked"), role, grantee.clone()),
        env.ledger().timestamp(),
    );
}

pub fn require_role(env: &Env, role: Role) -> Result<Address, Error> {
    let addr = if let Some(role_addr) = env
        .storage()
        .instance()
        .get::<DataKey, Address>(&DataKey::Role(role))
    {
        role_addr
    } else {
        require_admin(env)?
    };
    addr.require_auth();
    Ok(addr)
}

pub fn require_admin(env: &Env) -> Result<Address, Error> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)
}

pub fn has_role_internal(env: &Env, account: &Address, role: Role) -> bool {
    let granted: bool = env
        .storage()
        .persistent()
        .get(&DataKey::UserRole(account.clone(), role))
        .unwrap_or(false);
    if granted {
        return true;
    }
    let designated: Option<Address> = env.storage().instance().get(&DataKey::Role(role));
    match designated {
        Some(primary) => &primary == account,
        None => {
            env.storage()
                .instance()
                .get::<DataKey, Address>(&DataKey::Admin)
                .as_ref()
                == Some(account)
        }
    }
}

pub fn require_role_of(env: &Env, caller: &Address, role: Role) -> Result<(), Error> {
    if !has_role_internal(env, caller, role) {
        return Err(Error::RoleNotFound);
    }
    caller.require_auth();
    Ok(())
}

pub fn require_fee_authority(env: &Env) -> Result<(), Error> {
    if let Some(gov) = env
        .storage()
        .instance()
        .get::<DataKey, Address>(&DataKey::Governance)
    {
        gov.require_auth();
        Ok(())
    } else {
        require_role(env, Role::FeeManager)?;
        Ok(())
    }
}

// ── Circuit Breaker & Emergency Checks ───────────────────────────────────────

/// Read the current [`PauseLevel`] from storage.
///
/// For backward compatibility the legacy boolean `DataKey::Paused` is
/// consulted first: if it is `true` the effective level is
/// [`PauseLevel::FullHalt`]; otherwise the new `DataKey::PauseLevel` key is
/// used (defaulting to `PauseLevel::None` when absent).
pub fn get_pause_level(env: &Env) -> crate::types::PauseLevel {
    use crate::types::PauseLevel;
    // Legacy boolean migration path.
    let legacy: bool = env
        .storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false);
    if legacy {
        return PauseLevel::FullHalt;
    }
    let raw: u32 = env
        .storage()
        .instance()
        .get(&DataKey::PauseLevel)
        .unwrap_or(0u32);
    PauseLevel::from_u32(raw).unwrap_or(PauseLevel::None)
}

/// Legacy helper retained for call-sites that only need a yes/no "is anything
/// paused" answer. Returns `true` when the level is *not* `PauseLevel::None`.
pub fn is_paused_internal(env: &Env) -> bool {
    get_pause_level(env) != crate::types::PauseLevel::None
}

/// Reject the call if the contract is at [`PauseLevel::FullHalt`].
/// Direct payments are still allowed under [`PauseLevel::SwapsOnly`].
pub fn require_not_full_halt(env: &Env) -> Result<(), Error> {
    use crate::types::PauseLevel;
    if get_pause_level(env) == PauseLevel::FullHalt {
        return Err(Error::Paused);
    }
    Ok(())
}

/// Reject the call if any pause level is active (i.e. swaps are blocked under
/// both [`PauseLevel::SwapsOnly`] and [`PauseLevel::FullHalt`]).
pub fn require_swaps_unpaused(env: &Env) -> Result<(), Error> {
    use crate::types::PauseLevel;
    if get_pause_level(env) != PauseLevel::None {
        return Err(Error::Paused);
    }
    Ok(())
}

/// Kept for legacy call-sites that check "is the contract entirely halted".
pub fn require_circuit_closed(env: &Env) -> Result<(), Error> {
    require_not_full_halt(env)
}

pub fn is_frozen_internal(env: &Env) -> bool {
    env.storage()
        .instance()
        .get(&DataKey::Frozen)
        .unwrap_or(false)
}

// ── Multi-signature (M-of-N) Upgrade Helpers ─────────────────────────────────

pub fn load_multisig_config(env: &Env) -> Result<(Vec<Address>, u32), Error> {
    let threshold: u32 = env
        .storage()
        .instance()
        .get(&DataKey::MultisigThreshold)
        .ok_or(Error::MultisigNotInitialized)?;
    let signers: Vec<Address> = env
        .storage()
        .instance()
        .get(&DataKey::MultisigSigners)
        .ok_or(Error::MultisigNotInitialized)?;
    Ok((signers, threshold))
}

pub fn validate_multisig_config(signers: &Vec<Address>, threshold: u32) -> Result<u32, Error> {
    if signers.is_empty() {
        return Err(Error::InvalidMultisigConfig);
    }
    if threshold == 0 || threshold > signers.len() {
        return Err(Error::InvalidMultisigConfig);
    }
    for i in 0..signers.len() {
        for j in (i + 1)..signers.len() {
            if signers.get(i) == signers.get(j) {
                return Err(Error::InvalidMultisigConfig);
            }
        }
    }
    Ok(threshold)
}

pub fn load_upgrade_approvals(env: &Env, new_wasm_hash: &BytesN<32>) -> Vec<Address> {
    env.storage()
        .persistent()
        .get(&DataKey::UpgradeApproval(new_wasm_hash.clone()))
        .unwrap_or_else(|| Vec::new(env))
}

pub fn load_effective_approvals(
    env: &Env,
    signers: &Vec<Address>,
    new_wasm_hash: &BytesN<32>,
) -> Vec<Address> {
    let stored = load_upgrade_approvals(env, new_wasm_hash);
    let mut effective = Vec::new(env);
    for i in 0..stored.len() {
        if let Some(approver) = stored.get(i) {
            if signers.contains(&approver) {
                effective.push_back(approver);
            }
        }
    }
    effective
}

pub fn check_upgrade_authorized(env: &Env, new_wasm_hash: &BytesN<32>) -> Result<bool, Error> {
    let (signers, threshold) = load_multisig_config(env)?;
    Ok(load_effective_approvals(env, &signers, new_wasm_hash).len() >= threshold)
}

pub fn require_upgrade_authorized(env: &Env, new_wasm_hash: &BytesN<32>) -> Result<u32, Error> {
    let (signers, threshold) = load_multisig_config(env)?;
    let approvals = load_effective_approvals(env, &signers, new_wasm_hash);
    if approvals.len() < threshold {
        return Err(Error::InsufficientApprovals);
    }
    Ok(approvals.len())
}

pub fn apply_upgrade(env: &Env, new_wasm_hash: &BytesN<32>) {
    env.deployer()
        .update_current_contract_wasm(new_wasm_hash.clone());
    env.storage()
        .persistent()
        .remove(&DataKey::UpgradeApproval(new_wasm_hash.clone()));
}

pub fn store_multisig_config(env: &Env, signers: Vec<Address>, threshold: u32) {
    env.storage()
        .instance()
        .set(&DataKey::MultisigSigners, &signers);
    env.storage()
        .instance()
        .set(&DataKey::MultisigThreshold, &threshold);
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_LIFETIME_THRESHOLD, INSTANCE_BUMP_AMOUNT);
}

pub fn validate_queued_action(env: &Env, action: &ActionType) -> Result<(), Error> {
    match action {
        ActionType::Upgrade(new_wasm_hash) => {
            require_upgrade_authorized(env, new_wasm_hash)?;
        }
        ActionType::SetMultisigConfig(signers, threshold) => {
            validate_multisig_config(signers, *threshold)?;
        }
        _ => {}
    }
    Ok(())
}
