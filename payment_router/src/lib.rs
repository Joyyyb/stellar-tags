#![no_std]
pub mod access;
mod archival;
pub mod dead_mans_switch;
pub mod errors;
pub mod fees;
pub mod rate_limit;
pub mod storage;
pub mod timelock;
pub mod types;

pub use access::*;
use archival::{ArchiveLeaf, ArchiveMetadata, ArchiveRecordType};
pub use errors::*;
pub use fees::*;
pub use rate_limit::*;
pub use storage::*;
pub use timelock::*;
pub use types::*;

use soroban_sdk::xdr::ToXdr;
use soroban_sdk::{
    contract, contractimpl, log, symbol_short, token, vec, Address, Bytes, BytesN, Env,
    Error as SdkError, IntoVal, InvokeError, String, Symbol, Vec,
};

/// Soroban contract that routes token payments between addresses while
/// collecting a configurable platform fee, enforcing per-user daily spending
/// limits, and supporting an admin-managed blacklist and pause switch.
#[contract]
pub struct PaymentRouter;

#[contractimpl]
impl PaymentRouter {
    pub const BPS_DIVISOR: i128 = storage::BPS_DIVISOR;
    pub const XLM_DECIMALS: i128 = storage::XLM_DECIMALS;
    pub const MAX_AMOUNT: i128 = storage::MAX_AMOUNT;
    pub const DAILY_MAX_LIMIT: i128 = storage::DAILY_MAX_LIMIT;
    pub const VOLUME_THRESHOLD: i128 = storage::VOLUME_THRESHOLD;
    pub const SECONDS_IN_24H: u64 = storage::SECONDS_IN_24H;
    pub const MIN_DMS_TIMEOUT: u64 = storage::MIN_DMS_TIMEOUT;
    pub const DEFAULT_RATE_LIMIT: u32 = storage::DEFAULT_RATE_LIMIT;
    pub const MAX_RATE_LIMIT: u32 = storage::MAX_RATE_LIMIT;
    pub const CONTRACT_VERSION: &'static str = env!("CARGO_PKG_VERSION");
    pub const DEFAULT_MAX_SLIPPAGE_BPS: i128 = storage::DEFAULT_MAX_SLIPPAGE_BPS;
    pub const MAX_SLIPPAGE_BPS_LIMIT: i128 = storage::MAX_SLIPPAGE_BPS_LIMIT;
    pub const DAY_IN_LEDGERS: u32 = storage::DAY_IN_LEDGERS;
    pub const INSTANCE_BUMP_AMOUNT: u32 = storage::INSTANCE_BUMP_AMOUNT;
    pub const INSTANCE_LIFETIME_THRESHOLD: u32 = storage::INSTANCE_LIFETIME_THRESHOLD;
    pub const USER_BUMP_AMOUNT: u32 = storage::USER_BUMP_AMOUNT;
    pub const USER_LIFETIME_THRESHOLD: u32 = storage::USER_LIFETIME_THRESHOLD;
    pub const PERSISTENT_BUMP_AMOUNT: u32 = storage::PERSISTENT_BUMP_AMOUNT;
    pub const PERSISTENT_LIFETIME_THRESHOLD: u32 = storage::PERSISTENT_LIFETIME_THRESHOLD;

    // ── Delegated Domain Helpers ─────────────────────────────────────────────

    #[inline]
    fn set_role_internal(env: &Env, role: Role, account: &Address) {
        access::set_role_internal(env, role, account);
    }
    #[inline]
    fn remove_role_internal(env: &Env, role: Role, account: &Address) {
        access::remove_role_internal(env, role, account);
    }
    #[inline]
    fn apply_role_grant(env: &Env, grantee: &Address, role: Role) {
        access::apply_role_grant(env, grantee, role);
    }
    #[inline]
    fn apply_role_revoke(env: &Env, grantee: &Address, role: Role) {
        access::apply_role_revoke(env, grantee, role);
    }
    #[inline]
    fn require_role(env: &Env, role: Role) -> Result<Address, Error> {
        access::require_role(env, role)
    }
    #[inline]
    fn require_admin(env: &Env) -> Result<Address, Error> {
        access::require_admin(env)
    }
    #[inline]
    fn has_role_internal(env: &Env, account: &Address, role: Role) -> bool {
        access::has_role_internal(env, account, role)
    }
    #[inline]
    fn require_role_of(env: &Env, caller: &Address, role: Role) -> Result<(), Error> {
        access::require_role_of(env, caller, role)
    }
    #[inline]
    fn require_fee_authority(env: &Env) -> Result<(), Error> {
        access::require_fee_authority(env)
    }
    #[inline]
    fn load_multisig_config(env: &Env) -> Result<(Vec<Address>, u32), Error> {
        access::load_multisig_config(env)
    }
    #[inline]
    fn validate_multisig_config(signers: &Vec<Address>, threshold: u32) -> Result<u32, Error> {
        access::validate_multisig_config(signers, threshold)
    }
    #[inline]
    fn load_effective_approvals(
        env: &Env,
        signers: &Vec<Address>,
        new_wasm_hash: &BytesN<32>,
    ) -> Vec<Address> {
        access::load_effective_approvals(env, signers, new_wasm_hash)
    }
    #[inline]
    fn check_upgrade_authorized(env: &Env, new_wasm_hash: &BytesN<32>) -> Result<bool, Error> {
        access::check_upgrade_authorized(env, new_wasm_hash)
    }
    #[inline]
    fn require_upgrade_authorized(env: &Env, new_wasm_hash: &BytesN<32>) -> Result<u32, Error> {
        access::require_upgrade_authorized(env, new_wasm_hash)
    }
    #[inline]
    fn apply_upgrade(env: &Env, new_wasm_hash: &BytesN<32>) {
        access::apply_upgrade(env, new_wasm_hash);
    }
    #[inline]
    fn store_multisig_config(env: &Env, signers: Vec<Address>, threshold: u32) {
        access::store_multisig_config(env, signers, threshold);
    }
    #[inline]
    fn validate_queued_action(env: &Env, action: &ActionType) -> Result<(), Error> {
        access::validate_queued_action(env, action)
    }
    #[inline]
    fn load_fee_config(env: &Env) -> Result<(Address, i128, i128), Error> {
        fees::load_fee_config(env)
    }
    #[inline]
    fn load_user_record(env: &Env, sender: &Address, current_time: u64) -> (u64, i128, i128, bool) {
        storage::load_user_record(env, sender, current_time)
    }
    #[inline]
    fn accrue_user_record(env: &Env, sender: &Address, amount: i128) -> Result<i128, Error> {
        storage::accrue_user_record(env, sender, amount)
    }
    #[inline]
    fn is_frozen_internal(env: &Env) -> bool {
        access::is_frozen_internal(env)
    }
    #[inline]
    fn bump_heartbeat(env: &Env) {
        if env.storage().instance().has(&DataKey::BackupAdmin) {
            env.storage()
                .instance()
                .set(&DataKey::LastHeartbeat, &env.ledger().timestamp());
        }
    }
    #[inline]
    fn rate_limit_raw(env: &Env) -> u32 {
        rate_limit::rate_limit_raw(env)
    }
    #[inline]
    fn is_whitelisted(env: &Env, address: &Address) -> bool {
        rate_limit::is_whitelisted(env, address)
    }
    #[inline]
    fn rate_limit_used(env: &Env, sender: &Address) -> u32 {
        rate_limit::rate_limit_used(env, sender)
    }
    #[inline]
    fn check_rate_limit(env: &Env, sender: &Address) -> Result<(), Error> {
        rate_limit::check_rate_limit(env, sender)
    }
    #[inline]
    fn is_paused_internal(env: &Env) -> bool {
        access::is_paused_internal(env)
    }
    #[inline]
    fn require_circuit_closed(env: &Env) -> Result<(), Error> {
        access::require_circuit_closed(env)
    }
    #[inline]
    fn require_not_full_halt(env: &Env) -> Result<(), Error> {
        access::require_not_full_halt(env)
    }
    #[inline]
    fn require_swaps_unpaused(env: &Env) -> Result<(), Error> {
        access::require_swaps_unpaused(env)
    }
    #[inline]
    fn get_pause_level(env: &Env) -> PauseLevel {
        access::get_pause_level(env)
    }
    #[inline]
    fn calculate_fee(amount: i128, effective_fee_bps: i128, fee_cap: i128) -> (i128, i128) {
        fees::calculate_fee(amount, effective_fee_bps, fee_cap)
    }

    fn validate_swap_path(
        token_in: &Address,
        token_out: &Address,
        path: &Vec<Address>,
        min_amount_out: i128,
    ) -> Result<(), Error> {
        if path.is_empty() {
            return Err(Error::InvalidSwapPath);
        }
        if path.first().is_none_or(|a| a != *token_in) {
            return Err(Error::InvalidSwapPath);
        }
        if path.last().is_none_or(|a| a != *token_out) {
            return Err(Error::InvalidSwapPath);
        }
        if min_amount_out <= 0 {
            return Err(Error::InvalidSwapParams);
        }
        Ok(())
    }

    fn get_refund_balance_internal(env: &Env, user: &Address, token: &Address) -> i128 {
        let key = DataKey::RefundBalance(user.clone(), token.clone());
        env.storage().persistent().get(&key).unwrap_or(0)
    }

    pub(crate) fn credit_refund_balance(env: &Env, user: &Address, token: &Address, amount: i128) {
        let key = DataKey::RefundBalance(user.clone(), token.clone());
        let current_balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let new_balance = current_balance + amount;
        env.storage().persistent().set(&key, &new_balance);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_LIFETIME_THRESHOLD,
            Self::PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (symbol_short!("refunded"), user.clone(), token.clone()),
            amount,
        );
    }

    fn verify_kyc_for_amount(env: &Env, sender: &Address, amount: i128) -> Result<(), Error> {
        let threshold: Option<i128> = env.storage().instance().get(&DataKey::KycThreshold);
        if threshold.is_none() || amount <= threshold.unwrap_or(0) {
            return Ok(());
        }

        let oracle: Address = env
            .storage()
            .instance()
            .get(&DataKey::KycOracle)
            .ok_or(Error::KycRequired)?;
        if !KycOracleClient::new(env, &oracle).is_verified(sender) {
            return Err(Error::KycRequired);
        }
        Ok(())
    }

    /// Returns the configured swap slippage ceiling in basis points.
    fn max_slippage_bps(env: &Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::MaxSlippageBps)
            .unwrap_or(Self::DEFAULT_MAX_SLIPPAGE_BPS)
    }

    /// Returns whether a DEX router has been approved for swap routing.
    fn is_dex_registered_internal(env: &Env, dex: &Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::RegisteredDex(dex.clone()))
            .unwrap_or(false)
    }

    /// Invokes the DEX adapter and returns the amount it claims to have
    /// delivered.
    ///
    /// A reverting, missing, or mistyped DEX call is reported as
    /// [`Error::SwapFailed`] instead of trapping, so the caller can abort with
    /// a stable error code.  Because the error propagates out of the payment,
    /// the Soroban host reverts every balance and storage change made earlier
    /// in the same transaction.
    #[allow(clippy::too_many_arguments)]
    fn invoke_dex_swap(
        env: &Env,
        dex: &Address,
        sell_token: &Address,
        buy_token: &Address,
        amount_in: i128,
        min_amount_out: i128,
    ) -> Result<i128, Error> {
        let fn_name = Symbol::new(env, "swap");
        let args = vec![
            env,
            sell_token.clone().into_val(env),
            buy_token.clone().into_val(env),
            amount_in.into_val(env),
            min_amount_out.into_val(env),
            env.current_contract_address().into_val(env),
        ];

        let res: Result<Result<i128, SdkError>, Result<InvokeError, InvokeError>> =
            env.try_invoke_contract(dex, &fn_name, args);

        match res {
            Ok(Ok(amount_out)) => Ok(amount_out),
            Ok(Err(_)) | Err(_) => Err(Error::SwapFailed),
        }
    }

    /// Invokes the DEX adapter's quote for a read-only price check.
    ///
    /// Quotes never move funds, so a failure here is reported to the caller
    /// rather than trapping.
    fn invoke_dex_quote(
        env: &Env,
        dex: &Address,
        sell_token: &Address,
        buy_token: &Address,
        amount_in: i128,
    ) -> Result<i128, Error> {
        let args = vec![
            env,
            sell_token.clone().into_val(env),
            buy_token.clone().into_val(env),
            amount_in.into_val(env),
        ];
        let res: Result<Result<i128, SdkError>, Result<InvokeError, InvokeError>> =
            env.try_invoke_contract(dex, &Symbol::new(env, "quote"), args);

        match res {
            Ok(Ok(amount_out)) => Ok(amount_out),
            Ok(Err(_)) | Err(_) => Err(Error::SwapFailed),
        }
    }

    /// Returns the tightest `min_amount_out` that respects `max_slippage_bps`
    /// for a quote of `expected_amount_out`.
    fn slippage_floor(expected_amount_out: i128, max_slippage_bps: i128) -> i128 {
        let tolerated = (expected_amount_out * max_slippage_bps) / Self::BPS_DIVISOR;
        let floor = expected_amount_out - tolerated;
        if floor < 1 {
            1
        } else {
            floor
        }
    }

    /// Allocates and returns the next timelock nonce, incrementing the counter.
    fn next_nonce(env: &Env) -> u64 {
        let current: u64 = env
            .storage()
            .instance()
            .get(&DataKey::TimelockNonce)
            .unwrap_or(0u64);
        let next = current + 1;
        env.storage().instance().set(&DataKey::TimelockNonce, &next);
        next
    }

    /// Returns the current meta-transaction nonce for a user (`0` if never used).
    fn get_meta_nonce_internal(env: &Env, user: &Address) -> u64 {
        env.storage()
            .persistent()
            .get(&DataKey::MetaNonce(user.clone()))
            .unwrap_or(0)
    }

    /// Builds the domain-separated message for meta-transactions.
    /// Binds `current_contract_address` + `MetaPayment` struct + `signer_pubkey`.
    #[allow(clippy::too_many_arguments)]
    fn build_meta_message(
        env: &Env,
        sender: &Address,
        signer_pubkey: &BytesN<32>,
        recipient: &Address,
        token_address: &Address,
        amount: i128,
        nonce: u64,
        deadline: u64,
    ) -> Bytes {
        let mut payload = Bytes::new(env);
        payload.append(&env.current_contract_address().to_xdr(env));
        payload.append(&sender.to_xdr(env));
        payload.append(&Bytes::from_slice(env, &signer_pubkey.to_array()));
        payload.append(&recipient.to_xdr(env));
        payload.append(&token_address.to_xdr(env));
        payload.append(&amount.to_xdr(env));
        payload.append(&nonce.to_xdr(env));
        payload.append(&deadline.to_xdr(env));
        let hash = env.crypto().sha256(&payload);
        Bytes::from(&hash)
    }

    /// Core payment logic shared by `route_payment` and `route_payments`.
    ///
    /// The caller must have already obtained `sender`'s authorization, once
    /// per distinct sender across the batch (or via `require_auth` for a
    /// single payment). The transfers below re-check it inside the token
    /// contract, but doing it here first means an unauthorized payment is
    /// refused before any state is written.
    #[allow(clippy::too_many_arguments)]
    fn process_single_payment(
        env: &Env,
        sender: &Address,
        recipient: &Address,
        token_address: &Address,
        amount: i128,
        platform_treasury: &Address,
        fee_bps: i128,
        fee_cap: i128,
    ) -> Result<(), Error> {
        env.events().publish(
            (Symbol::new(env, "payment_initiated"), sender.clone()),
            amount,
        );

        // Validations moved to route_payments to prevent rollback panic on Windows testutils

        // Roll the 24-hour window forward and record the lifetime volume in a
        // single packed write (issue #663). The returned volume is the
        // pre-payment one, which is what the tiered discount keys off.
        let user_volume = Self::accrue_user_record(env, sender, amount)?;
        let effective_fee_bps = if user_volume > Self::VOLUME_THRESHOLD {
            fee_bps / 2
        } else {
            fee_bps
        };
        // Verify sender has sufficient balance
        let token_client = token::Client::new(env, token_address);
        if token_client.balance(sender) < amount {
            return Err(Error::InsufficientBalance);
        }

        // Calculate fee
        let mut fee_amount = amount.saturating_mul(effective_fee_bps) / Self::BPS_DIVISOR;
        if fee_amount > fee_cap {
            fee_amount = fee_cap;
        }
        if fee_amount > amount {
            fee_amount = amount;
        }
        let remainder = amount - fee_amount;

        // Execute transfers safely without panics
        if fee_amount > 0
            && token_client
                .try_transfer(sender, platform_treasury, &fee_amount)
                .is_err()
        {
            return Err(Error::LimitExceeded);
        }
        if remainder > 0 {
            // Attempt to transfer remainder directly to recipient.
            // If recipient cannot receive tokens (e.g. missing trustline or rejection),
            // transfer funds into the contract and credit the sender's internal refund ledger.
            match token_client.try_transfer(sender, recipient, &remainder) {
                Ok(Ok(())) => {
                    log!(env, "Remaining balance routed to recipient");
                }
                _ => {
                    log!(
                        env,
                        "Recipient transfer failed; crediting sender refund balance"
                    );
                    if let Ok(Ok(())) = token_client.try_transfer(
                        sender,
                        &env.current_contract_address(),
                        &remainder,
                    ) {
                        Self::credit_refund_balance(env, sender, token_address, remainder);
                    } else {
                        return Err(Error::LimitExceeded);
                    }
                }
            }
        }

        // Emit routed event
        env.events().publish(
            (symbol_short!("routed"), sender.clone(), recipient.clone()),
            amount,
        );

        log!(env, "Platform fee routed to treasury");

        Ok(())
    }

    /// Allowance-based variant for meta-transactions.
    /// Skips `sender.require_auth()`; funds move via `transfer_from` using
    /// allowance previously granted to the router contract, so a relayer
    /// can submit on the user's behalf after signature verification.
    #[allow(clippy::too_many_arguments)]
    fn process_single_payment_no_auth(
        env: &Env,
        sender: &Address,
        recipient: &Address,
        token_address: &Address,
        amount: i128,
        platform_treasury: &Address,
        fee_bps: i128,
        fee_cap: i128,
    ) -> Result<(), Error> {
        env.events().publish(
            (Symbol::new(env, "payment_initiated"), sender.clone()),
            amount,
        );

        if sender == recipient {
            return Err(Error::InvalidRecipient);
        }

        if Self::is_blacklisted(env.clone(), recipient.clone()) {
            return Err(Error::Blacklisted);
        }

        let max_amount: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MaxAmount)
            .unwrap_or(Self::MAX_AMOUNT);
        if amount <= 0 || amount > max_amount {
            return Err(Error::LimitExceeded);
        }

        let min_limit: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MinLimit)
            .unwrap_or(0);
        if amount < min_limit {
            return Err(Error::LimitExceeded);
        }

        Self::verify_kyc_for_amount(env, sender, amount)?;

        // Single packed write covering the 24-hour window and the lifetime
        // volume (issue #663); the discount keys off the pre-payment volume.
        let user_volume = Self::accrue_user_record(env, sender, amount)?;
        let effective_fee_bps = if user_volume > Self::VOLUME_THRESHOLD {
            fee_bps / 2
        } else {
            fee_bps
        };

        let router = env.current_contract_address();
        let token_client = token::Client::new(env, token_address);
        if token_client.balance(sender) < amount {
            return Err(Error::InsufficientBalance);
        }
        if token_client.allowance(sender, &router) < amount {
            return Err(Error::InsufficientBalance);
        }

        // Calculate fee
        let fee_product = amount.checked_mul(effective_fee_bps).unwrap_or(amount);
        let mut fee_amount = fee_product / Self::BPS_DIVISOR;
        if fee_amount > fee_cap {
            fee_amount = fee_cap;
        }
        if fee_amount > amount {
            fee_amount = amount;
        }
        let remainder = amount - fee_amount;

        // Execute transfers via allowance without panics
        if fee_amount > 0
            && token_client
                .try_transfer_from(&router, sender, platform_treasury, &fee_amount)
                .is_err()
        {
            return Err(Error::LimitExceeded);
        }
        if remainder > 0 {
            match token_client.try_transfer_from(&router, sender, recipient, &remainder) {
                Ok(Ok(())) => {
                    log!(env, "Remaining balance routed to recipient");
                }
                _ => {
                    log!(
                        env,
                        "Recipient transfer failed; crediting sender refund balance"
                    );
                    if let Ok(Ok(())) =
                        token_client.try_transfer_from(&router, sender, &router, &remainder)
                    {
                        Self::credit_refund_balance(env, sender, token_address, remainder);
                    } else {
                        return Err(Error::LimitExceeded);
                    }
                }
            }
        }

        env.events().publish(
            (symbol_short!("routed"), sender.clone(), recipient.clone()),
            amount,
        );

        log!(env, "Platform fee routed to treasury");

        Ok(())
    }

    // ── Public contract methods ──────────────────────────────────────────────

    /// Circuit-breaker guard applied to every non-essential operation.
    ///
    /// While the pause switch is engaged all operational state changes —
    /// payments, timelock queue/execute, fee/treasury/governance/min-limit
    /// configuration, treasury yield movements and token recovery — are
    /// rejected with `Error::Paused`.
    ///
    /// One-time setup: records the admin and the initial fee configuration
    /// in instance storage. Must be called before `route_payment`.
    ///
    /// # Parameters
    /// * `env` - The Soroban environment interface.
    /// * `sender` - The address initiating the payment. Must authorize the transaction.
    /// * `recipient` - The destination address for the payment (e.g., the Anchor's wallet for fiat withdrawals).
    /// * `platform_treasury` - The address where the platform fee will be deposited.
    /// * `token_address` - The contract ID of the token asset being transferred (e.g., NGNC or USDC).
    /// * `amount` - The total amount of tokens to be routed (inclusive of the fee).
    pub fn initialize(
        env: Env,
        admin: Address,
        platform_treasury: Address,
        fee_bps: i128,
        fee_cap: i128,
        max_amount: i128,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::PlatformTreasury, &platform_treasury);
        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        env.storage().instance().set(&DataKey::FeeCap, &fee_cap);
        env.storage()
            .instance()
            .set(&DataKey::MaxAmount, &max_amount);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().instance().set(&DataKey::Frozen, &false);
        env.storage()
            .instance()
            .set(&DataKey::MaxSlippageBps, &Self::DEFAULT_MAX_SLIPPAGE_BPS);
        env.storage().instance().set(&DataKey::TimelockNonce, &0u64);
        // RBAC Initialization: assign initial admin to all operational roles
        Self::set_role_internal(&env, Role::SuperAdmin, &admin);
        Self::set_role_internal(&env, Role::TreasuryManager, &admin);
        Self::set_role_internal(&env, Role::ComplianceOfficer, &admin);
        Self::set_role_internal(&env, Role::FeeManager, &admin);
        Self::set_role_internal(&env, Role::Pauser, &admin);

        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        Ok(())
    }

    // ── Role-Based Access Control (RBAC) ────────────────────────────────────

    /// Grants an operational role to `grantee`.
    ///
    /// Only an address holding `SuperAdmin` may call this.  The `admin`
    /// parameter is the address expected to authorize the transaction, and
    /// `admin.require_auth()` is always invoked, so a caller that cannot supply
    /// that signature is rejected even if the role would otherwise resolve.
    ///
    /// # Parameters
    /// - `admin`: Address expected to authorize the call; must hold `SuperAdmin`.
    /// - `grantee`: Address to receive the role.
    /// - `role`: The `Role` variant to grant.
    ///
    /// # Returns
    /// `Ok(())` on success, `Err(Error::RoleNotFound)` if `admin` does not hold
    /// `SuperAdmin`, or `Err(Error::NotInitialized)` if the contract has no
    /// admin set yet.
    ///
    /// # Panics
    /// Panics if `admin` does not authorize the call.
    ///
    /// Granting a role the grantee already holds is a no-op that emits
    /// `role_assigned` again rather than an error.  Because a role has a single
    /// designated holder, granting it to a new address revokes it from the
    /// previous one.
    pub fn grant_role(env: Env, admin: Address, grantee: Address, role: Role) -> Result<(), Error> {
        Self::require_role_of(&env, &admin, Role::SuperAdmin)?;
        Self::apply_role_grant(&env, &grantee, role);
        Ok(())
    }

    /// Revokes an operational role from `grantee`.
    ///
    /// Only an address holding `SuperAdmin` may call this.  As with
    /// [`Self::grant_role`], `admin.require_auth()` is always invoked.
    ///
    /// # Parameters
    /// - `admin`: Address expected to authorize the call; must hold `SuperAdmin`.
    /// - `grantee`: Address from which the role will be revoked.
    /// - `role`: The `Role` variant to revoke.
    ///
    /// # Returns
    /// `Ok(())` on success, `Err(Error::RoleNotFound)` if `admin` does not hold
    /// `SuperAdmin`, `Err(Error::InvalidRole)` if the call would revoke the
    /// acting `SuperAdmin`'s own root role, or `Err(Error::NotInitialized)`.
    ///
    /// # Panics
    /// Panics if `admin` does not authorize the call.
    ///
    /// Revoking a role the grantee never held is a no-op that emits
    /// `role_revoked` rather than an error, so revocations are idempotent and
    /// safe to retry.
    pub fn revoke_role(
        env: Env,
        admin: Address,
        grantee: Address,
        role: Role,
    ) -> Result<(), Error> {
        Self::require_role_of(&env, &admin, Role::SuperAdmin)?;
        if role == Role::SuperAdmin && admin == grantee {
            return Err(Error::InvalidRole);
        }
        Self::apply_role_revoke(&env, &grantee, role);
        Ok(())
    }

    /// Grants an operational role to an account. SuperAdmin-protected.
    ///
    /// Retained for compatibility with the published bindings; this is
    /// [`Self::grant_role`] with the `admin` argument resolved by the contract
    /// instead of supplied by the caller.
    ///
    /// # Parameters
    /// - `account`: Target address to receive the role.
    /// - `role`: The `Role` variant to grant.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if uninitialized.
    ///
    /// # Panics
    /// Panics if the current `SuperAdmin` does not authorize the call.
    pub fn assign_role(env: Env, account: Address, role: Role) -> Result<(), Error> {
        // The resolved address is discarded: the point is the `require_auth()`
        // that `require_role` performs on it.
        Self::require_role(&env, Role::SuperAdmin)?;
        Self::apply_role_grant(&env, &account, role);
        Ok(())
    }

    /// Queries whether a given account holds an active role assignment.
    ///
    /// Read-only and authorization-free.  Reports effective authority: the
    /// account is granted the role directly, is the role's designated holder,
    /// or is the root admin standing in for a role that has not been delegated.
    ///
    /// # Parameters
    /// - `account`: Address to query.
    /// - `role`: Role variant to check.
    ///
    /// # Returns
    /// `true` if the account can exercise `role`, `false` otherwise.
    pub fn has_role(env: Env, account: Address, role: Role) -> bool {
        Self::has_role_internal(&env, &account, role)
    }

    /// Returns the primary designated member address for a role, if one is configured.
    ///
    /// # Parameters
    /// - `role`: The role variant to query.
    ///
    /// # Returns
    /// `Some(Address)` if set, or `None` if unassigned.
    pub fn get_role_member(env: Env, role: Role) -> Option<Address> {
        env.storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::Role(role))
    }

    /// Returns the administrative role governing the specified role.
    ///
    /// In this RBAC architecture, `SuperAdmin` governs all operational roles.
    pub fn get_role_admin(_env: Env, _role: Role) -> Role {
        Role::SuperAdmin
    }

    // ── Timelock: queue / execute / cancel ───────────────────────────────────

    /// Queues an admin action to be executed after a 24-hour delay.
    ///
    /// The admin provides the desired `ActionType` variant and receives a
    /// numeric nonce identifying this entry. Pass this nonce to `execute_action`
    /// after 24 hours, or to `cancel_action` to abort the intent.
    ///
    /// Sensitive parameter changes must go through the timelock.
    /// The contract must not be frozen, and the admin must authorize.
    pub fn queue_action(env: Env, action: ActionType) -> Result<u64, Error> {
        if Self::is_frozen_internal(&env) {
            return Err(Error::ContractFrozen);
        }
        // Circuit breaker: queuing a parameter change is a non-essential state
        // change and is blocked while paused.
        Self::require_circuit_closed(&env)?;

        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        let nonce = Self::next_nonce(&env);
        let queued_at = env.ledger().timestamp();

        let entry = TimelockEntry {
            queued_at,
            action: action.clone(),
        };

        let key = DataKey::TimelockEntry(nonce);
        env.storage().persistent().set(&key, &entry);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_LIFETIME_THRESHOLD,
            Self::PERSISTENT_BUMP_AMOUNT,
        );

        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events().publish(
            (Symbol::new(&env, "action_queued"), admin),
            (nonce, queued_at),
        );

        log!(&env, "Timelock action queued with nonce {}", nonce);
        Ok(nonce)
    }

    /// Returns the pending `TimelockEntry` for the given nonce, or an error if
    /// it does not exist.
    pub fn get_queued_action(env: Env, nonce: u64) -> Result<TimelockEntry, Error> {
        let key = DataKey::TimelockEntry(nonce);
        env.storage()
            .persistent()
            .get(&key)
            .ok_or(Error::TimelockNotFound)
    }

    /// Executes a previously queued action identified by `nonce`.
    ///
    /// Requirements:
    /// - The contract must not be frozen.
    /// - The admin must authorize.
    /// - The entry identified by `nonce` must exist.
    /// - At least 24 hours (`SECONDS_IN_24H`) must have passed since queuing.
    /// - For [`ActionType::Upgrade`], the multi-signature threshold must
    ///   already be met for that WASM hash.
    ///
    /// On success the entry is removed and the underlying setter is invoked.
    pub fn execute_action(env: Env, nonce: u64) -> Result<(), Error> {
        timelock::execute_action(&env, nonce)
    }

    /// Executes a batch of previously queued timelock actions sequentially within
    /// a single atomic invocation.
    ///
    /// Requirements:
    /// - The contract must not be frozen.
    /// - Circuit breaker must be closed.
    /// - The admin must authorize.
    /// - All nonces in the batch must be verified ready and valid BEFORE any action is executed.
    /// - All actions execute sequentially. If any action fails execution,
    ///   the entire invocation reverts atomically.
    /// - Emits `batch_actions_executed` event with the list of nonces.
    pub fn execute_batch_actions(env: Env, nonces: Vec<u64>) -> Result<(), Error> {
        timelock::execute_batch_actions(&env, nonces)
    }

    /// Cancels a pending timelock entry before it can be executed.
    ///
    /// This is the primary defence when a compromised admin has queued a
    /// malicious action: any other admin (after a key rotation) or a
    /// multi-sig governance can cancel it within the 24-hour window.
    ///
    /// Admin authorization is required. The contract may be frozen.
    pub fn cancel_action(env: Env, nonce: u64) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        let key = DataKey::TimelockEntry(nonce);
        if !env.storage().persistent().has(&key) {
            return Err(Error::TimelockNotFound);
        }

        env.storage().persistent().remove(&key);

        env.events()
            .publish((Symbol::new(&env, "action_cancelled"), admin), nonce);

        log!(&env, "Timelock action cancelled for nonce {}", nonce);
        Ok(())
    }

    // ── Freeze / unfreeze ────────────────────────────────────────────────────

    /// Instantly freezes the contract, blocking all payments and timelock
    /// executions.  This is the emergency last resort when an admin key is
    /// known to be compromised.
    ///
    /// Unlike other sensitive admin operations, freeze takes effect immediately
    /// — it does NOT go through the timelock — so it is always available as a
    /// rapid-response tool.
    ///
    /// Admin authorization is required.
    pub fn emergency_freeze(env: Env) -> Result<(), Error> {
        let super_admin = Self::require_role(&env, Role::SuperAdmin)?;

        env.storage().instance().set(&DataKey::Frozen, &true);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events().publish(
            (Symbol::new(&env, "emergency_freeze"), super_admin),
            env.ledger().timestamp(),
        );

        log!(&env, "Contract frozen by SuperAdmin");
        Ok(())
    }

    /// Removes the frozen state, restoring normal contract operation.
    ///
    /// Like `emergency_freeze`, this takes effect immediately and does not
    /// go through the timelock.
    ///
    /// SuperAdmin authorization is required.
    pub fn unfreeze(env: Env) -> Result<(), Error> {
        let super_admin = Self::require_role(&env, Role::SuperAdmin)?;

        env.storage().instance().set(&DataKey::Frozen, &false);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events().publish(
            (Symbol::new(&env, "unfreeze"), super_admin),
            env.ledger().timestamp(),
        );

        log!(&env, "Contract unfrozen by SuperAdmin");
        Ok(())
    }

    // ── Rate limiting (issue #716) ───────────────────────────────────────

    /// Sets the maximum number of payment invocations a single sender may
    /// make per ledger sequence. Admin-only.
    ///
    /// Rate limiting is active immediately after `initialize` with a default
    /// cap of 10 invocations per address per ledger. This setter changes the
    /// cap; a value of `0` disables rate limiting entirely (whitelisting
    /// individual addresses via `set_rate_limit_whitelist` is usually
    /// preferable to disabling the limiter for everyone).
    ///
    /// # Parameters
    /// - `max_calls_per_ledger`: Invocation cap per address per ledger.
    ///   Must not exceed `MAX_RATE_LIMIT` (1,000,000).
    ///
    /// # Returns
    /// `Ok(())` on success, `Err(Error::NotInitialized)` if the contract has
    /// no admin set yet, or `Err(Error::LimitExceeded)` if the cap exceeds
    /// `MAX_RATE_LIMIT`.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call.
    pub fn set_rate_limit(env: Env, max_calls_per_ledger: u32) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        if max_calls_per_ledger > Self::MAX_RATE_LIMIT {
            return Err(Error::LimitExceeded);
        }

        env.storage()
            .instance()
            .set(&DataKey::RateLimitConfig, &max_calls_per_ledger);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events().publish(
            (Symbol::new(&env, "rate_limit_set"), admin),
            max_calls_per_ledger,
        );

        log!(&env, "Rate limit updated");
        Ok(())
    }

    /// Whitelists an address, exempting it from the per-ledger invocation cap
    /// so legitimate high-volume senders are never throttled. Admin-only.
    ///
    /// # Parameters
    /// - `address`: Sender to exempt from rate limiting.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call.
    pub fn set_rate_limit_whitelist(env: Env, address: Address) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        let key = DataKey::RateLimitWhitelist(address.clone());
        env.storage().persistent().set(&key, &true);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_LIFETIME_THRESHOLD,
            Self::PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (Symbol::new(&env, "rate_limit_whitelist_added"), address),
            env.ledger().timestamp(),
        );

        log!(&env, "Address whitelisted from rate limiting");
        Ok(())
    }

    /// Removes an address from the rate-limit whitelist, restoring the
    /// standard per-ledger cap for it. Admin-only.
    ///
    /// # Parameters
    /// - `address`: Sender to remove from the whitelist.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call.
    pub fn remove_rate_limit_whitelist(env: Env, address: Address) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        env.storage()
            .persistent()
            .remove(&DataKey::RateLimitWhitelist(address.clone()));

        env.events().publish(
            (Symbol::new(&env, "rate_limit_whitelist_removed"), address),
            env.ledger().timestamp(),
        );

        log!(&env, "Address removed from rate-limit whitelist");
        Ok(())
    }

    /// Returns the active per-ledger invocation cap. A value of `0` means
    /// rate limiting is disabled.
    ///
    /// # Returns
    /// The configured cap, or the default cap (10) when no explicit
    /// configuration has been stored.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_rate_limit(env: Env) -> u32 {
        Self::rate_limit_raw(&env)
    }

    /// Returns whether an address is exempt from the rate limiter.
    ///
    /// # Parameters
    /// - `address`: Address to check.
    ///
    /// # Returns
    /// `true` if the address is on the rate-limit whitelist.
    ///
    /// # Panics
    /// Does not panic.
    pub fn is_rate_limit_whitelisted(env: Env, address: Address) -> bool {
        Self::is_whitelisted(&env, &address)
    }

    /// Returns how many more payment invocations `sender` can make in the
    /// current ledger under the active cap.
    ///
    /// # Parameters
    /// - `sender`: Address whose remaining allowance to compute.
    ///
    /// # Returns
    /// Remaining invocations in the current ledger window, saturating at 0
    /// once the cap is reached. Whitelisted senders (and a disabled limiter)
    /// report `u32::MAX`.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_rate_limit_remaining(env: Env, sender: Address) -> u32 {
        let limit = Self::rate_limit_raw(&env);
        if limit == 0 || Self::is_whitelisted(&env, &sender) {
            return u32::MAX;
        }

        // Both operands are `u32`: the cap is read from `RateLimitConfig` and
        // the counter from the packed `BytesN<8>`, so the remainder is at most
        // the cap and needs no widening. `saturating_sub` pins it at 0 if a
        // counter were somehow ahead of the cap.
        limit.saturating_sub(Self::rate_limit_used(&env, &sender))
    }

    /// Returns whether the contract is currently frozen.
    pub fn is_frozen(env: Env) -> bool {
        Self::is_frozen_internal(&env)
    }

    // ── Sensitive admin setters (now require timelock) ───────────────────────
    //
    // The functions below are intentionally kept as thin wrappers that apply
    // the change *directly* but only when called from execute_action (i.e.
    // after the timelock has been satisfied).  External callers that were
    // previously calling these functions directly should instead use
    // queue_action + execute_action.
    //
    // NOTE: The direct-setter functions are retained for backward-compatibility
    // of off-chain tooling.  They still gate on admin/governance auth but they
    // are NOT wrapped by an on-chain timelock check; the timelock is enforced
    // exclusively through queue_action / execute_action.

    /// Updates the treasury address that receives the platform fee.
    ///
    /// Updates the treasury address that receives the platform fee. Protected by TreasuryManager.
    ///
    /// # Parameters
    /// - `new_treasury`: Address to receive platform fees going forward.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the current TreasuryManager does not authorize the call.
    ///
    /// DEPRECATED for direct use.  Queue via `queue_action(ActionType::SetPlatformTreasury(…))`
    /// and execute after 24 hours.  This direct path is retained for tooling
    /// compatibility only.
    pub fn set_platform_treasury(env: Env, new_treasury: Address) -> Result<(), Error> {
        // Circuit breaker: platform parameter changes are non-essential.
        Self::require_circuit_closed(&env)?;
        Self::require_role(&env, Role::TreasuryManager)?;

        env.storage()
            .instance()
            .set(&DataKey::PlatformTreasury, &new_treasury);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );
        Ok(())
    }

    /// Updates the fee basis points and fee cap.
    /// Requires governance authority if a governance address is set; otherwise admin-only.
    ///
    /// # Parameters
    /// - `fee_bps`: New platform fee rate, in basis points.
    /// - `fee_cap`: New maximum fee taken from a single payment.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the caller does not authorize the call.
    ///
    /// DEPRECATED for direct use.  Queue via `queue_action(ActionType::SetFeeConfig(…))`.
    pub fn set_fee_config_legacy(env: Env, fee_bps: i128, fee_cap: i128) -> Result<(), Error> {
        // Circuit breaker: fee parameter changes are non-essential.
        Self::require_circuit_closed(&env)?;
        Self::require_fee_authority(&env)?;

        env.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        env.storage().instance().set(&DataKey::FeeCap, &fee_cap);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );
        Ok(())
    }

    /// Alias for `set_fee_config_legacy`. Admin-only.
    ///
    /// # Parameters
    /// - `fee_bps`: New platform fee rate, in basis points.
    /// - `fee_cap`: New maximum fee taken from a single payment.
    ///
    /// # Returns
    /// See `set_fee_config_legacy`.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call.
    ///
    /// DEPRECATED for direct use.  Queue via `queue_action(ActionType::SetFeeConfig(…))`.
    pub fn set_fee_config(env: Env, fee_bps: i128, fee_cap: i128) -> Result<(), Error> {
        Self::set_fee_config_legacy(env, fee_bps, fee_cap)
    }

    /// Updates the fee basis points.
    /// Requires governance authority if a governance address is set; otherwise admin-only.
    ///
    /// # Parameters
    /// - `new_fee_bps`: New platform fee rate, in basis points.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the caller does not authorize the call.
    ///
    /// DEPRECATED for direct use.  Queue via `queue_action(ActionType::SetFeeBps(…))`.
    pub fn set_fee_bps(env: Env, new_fee_bps: i128) -> Result<(), Error> {
        // Circuit breaker: fee parameter changes are non-essential.
        Self::require_circuit_closed(&env)?;
        Self::require_fee_authority(&env)?;

        env.storage().instance().set(&DataKey::FeeBps, &new_fee_bps);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );
        Ok(())
    }

    /// Sets the governance contract address. After this call, only the governance
    /// contract can update fees. Admin-only — can only be set once per governance cycle.
    ///
    /// DEPRECATED for direct use.  Queue via `queue_action(ActionType::SetGovernance(…))`.
    pub fn set_governance(env: Env, gov: Address) -> Result<(), Error> {
        // Circuit breaker: governance parameter changes are non-essential.
        Self::require_circuit_closed(&env)?;
        Self::require_role(&env, Role::SuperAdmin)?;
        env.storage().instance().set(&DataKey::Governance, &gov);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );
        Ok(())
    }

    /// Configures the DAO token and minimum voting weight for fee proposals.
    /// This administrative bootstrap does not itself change fees; subsequent
    /// fee changes can be made through the proposal lifecycle.
    pub fn configure_governance(
        env: Env,
        governance_token: Address,
        quorum: i128,
    ) -> Result<(), Error> {
        Self::require_role(&env, Role::SuperAdmin)?;
        if quorum <= 0 {
            return Err(Error::InvalidProposal);
        }
        env.storage()
            .instance()
            .set(&DataKey::GovernanceToken, &governance_token);
        env.storage()
            .instance()
            .set(&DataKey::GovernanceQuorum, &quorum);
        Ok(())
    }

    /// Creates a fee proposal weighted by governance-token balances.
    pub fn propose_fee_change(
        env: Env,
        proposer: Address,
        fee_bps: i128,
        fee_cap: i128,
        voting_period: u64,
    ) -> Result<u64, Error> {
        proposer.require_auth();
        let governance_token: Address = env
            .storage()
            .instance()
            .get(&DataKey::GovernanceToken)
            .ok_or(Error::GovernanceNotConfigured)?;
        if token::Client::new(&env, &governance_token).balance(&proposer) <= 0 {
            return Err(Error::GovernanceNotConfigured);
        }
        if !(0..=10_000).contains(&fee_bps) || fee_cap < 0 || voting_period == 0 {
            return Err(Error::InvalidProposal);
        }
        let nonce: u64 = env
            .storage()
            .instance()
            .get(&DataKey::GovernanceNonce)
            .unwrap_or(0);
        let id = nonce.saturating_add(1);
        env.storage().instance().set(&DataKey::GovernanceNonce, &id);
        let quorum = env
            .storage()
            .instance()
            .get(&DataKey::GovernanceQuorum)
            .unwrap_or(0);
        env.storage().persistent().set(
            &DataKey::GovernanceProposal(id),
            &FeeProposal {
                proposer,
                fee_bps,
                fee_cap,
                created_at: env.ledger().timestamp(),
                voting_ends_at: env.ledger().timestamp().saturating_add(voting_period),
                yes_votes: 0,
                no_votes: 0,
                quorum,
                executed: false,
            },
        );
        Ok(id)
    }

    pub fn get_fee_proposal(env: Env, proposal_id: u64) -> Option<FeeProposal> {
        env.storage()
            .persistent()
            .get(&DataKey::GovernanceProposal(proposal_id))
    }

    /// Sets the minimum allowed routing amount. FeeManager-protected.
    ///
    /// # Parameters
    /// - `min_limit`: Smallest `amount` that `route_payment` /
    ///   `route_payments` will accept going forward.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the current FeeManager does not authorize the call.
    ///
    /// DEPRECATED for direct use.  Queue via `queue_action(ActionType::SetMinLimit(…))`.
    pub fn set_min_limit(env: Env, min_limit: i128) -> Result<(), Error> {
        // Circuit breaker: routing limit changes are non-essential.
        Self::require_circuit_closed(&env)?;
        Self::require_role(&env, Role::FeeManager)?;

        env.storage().instance().set(&DataKey::MinLimit, &min_limit);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );
        Ok(())
    }

    /// Returns the current protocol fee percentage in basis points.
    ///
    /// # Returns
    /// The configured `fee_bps`, or `0` if the contract has not been
    /// initialized.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_fee(env: Env) -> i128 {
        env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0)
    }

    /// Pauses or unpauses the payment router. Pauser-protected.
    ///
    /// # Parameters
    /// - `paused`: `true` to reject `route_payment` / `route_payments`
    ///   calls, `false` to allow them again.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the current Pauser does not authorize the call.
    ///
    /// This is NOT timelocked — operational pausing must remain instant.
    ///
    /// # Parameters (updated for issue #890)
    /// - `level`: The [`PauseLevel`] to apply. Pass `0` for `None` (resume),
    ///   `1` for `SwapsOnly`, or `2` for `FullHalt`.
    ///
    /// Stores the level in `DataKey::PauseLevel` and clears the legacy
    /// `DataKey::Paused` boolean so old deployments are not confused.
    pub fn set_pause(env: Env, level: u32) -> Result<(), Error> {
        Self::require_role(&env, Role::Pauser)?;

        let pause_level = PauseLevel::from_u32(level).ok_or(Error::InvalidFeeRate)?;

        // Write the tiered level and clear the legacy boolean key so both
        // paths always agree.
        env.storage()
            .instance()
            .set(&DataKey::PauseLevel, &level);
        env.storage().instance().remove(&DataKey::Paused);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events()
            .publish((Symbol::new(&env, "pause_level"),), (level,));

        let _ = pause_level; // used via PauseLevel::from_u32 validation above
        Ok(())
    }

    /// Backward-compatible alias. `true` maps to `FullHalt`, `false` to `None`.
    ///
    /// Pauser-protected.
    pub fn set_paused(env: Env, paused: bool) -> Result<(), Error> {
        let level: u32 = if paused { 2 } else { 0 };
        Self::set_pause(env, level)
    }

    /// Returns whether the contract is currently paused at any level.
    ///
    /// # Returns
    /// `true` if the pause level is not `None`.
    ///
    /// # Panics
    /// Does not panic.
    pub fn is_paused(env: Env) -> bool {
        Self::is_paused_internal(&env)
    }

    /// Returns the current tiered pause level as a `u32` discriminant
    /// (0 = None, 1 = SwapsOnly, 2 = FullHalt). Introduced by issue #890.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_pause_level_value(env: Env) -> u32 {
        Self::get_pause_level(&env) as u32
    }

    /// Returns the cumulative amount a given sender has routed through the contract.
    ///
    /// # Parameters
    /// - `user`: Sender address to look up.
    ///
    /// # Returns
    /// The lifetime routed volume for `user`, or `0` if they have never
    /// routed a payment.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_user_volume(env: Env, user: Address) -> i128 {
        Self::get_user_record(env, user).volume
    }

    /// Returns a sender's combined routing record: the amount accumulated in
    /// the current 24-hour window and their cumulative lifetime volume
    /// (issue #663).
    ///
    /// Reads the single packed `UserRecord` entry. For a sender that only has
    /// the legacy pre-#663 split entries, both counters are combined from
    /// those without writing anything.
    ///
    /// # Parameters
    /// - `user`: Sender address to look up.
    ///
    /// # Returns
    /// A [`UserRecord`] with zeroed counters if `user` has never routed a
    /// payment; `last_reset_time` is then the current ledger timestamp.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_user_record(env: Env, user: Address) -> UserRecord {
        let current_time = env.ledger().timestamp();
        let (last_reset_time, accumulated_amount, volume, _) =
            Self::load_user_record(&env, &user, current_time);

        UserRecord {
            accumulated_amount,
            volume,
            last_reset_time,
        }
    }

    /// Permissionless migration of a sender's legacy pre-#663 split entries
    /// (`UserSpending` + `UserVolume`) into the single packed `UserRecord`
    /// (issue #663).
    ///
    /// Callable by anyone: it only recombines values that are already on the
    /// ledger and never invents or destroys value. When the sender's packed
    /// record was already created by a recent payment, this just removes the
    /// stale legacy keys and keeps the newer packed values.
    ///
    /// # Parameters
    /// - `user`: The sender whose legacy entries should be migrated.
    ///
    /// # Returns
    /// `true` if legacy state was found and migrated, `false` if `user` has
    /// no legacy entries to migrate.
    ///
    /// # Panics
    /// Does not panic.
    pub fn migrate_user_record(env: Env, user: Address) -> bool {
        let record_key = DataKey::UserRecord(user.clone());
        let has_packed = env.storage().persistent().has(&record_key);

        let legacy_spending: Option<(u64, i128)> = env
            .storage()
            .persistent()
            .get(&DataKey::UserSpending(user.clone()))
            .map(|packed: BytesN<24>| unpack_legacy_spending(&packed));
        let legacy_volume: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::UserVolume(user.clone()))
            .unwrap_or(0);

        if legacy_spending.is_none() && legacy_volume == 0 {
            return false;
        }

        if !has_packed {
            let (last_reset_time, accumulated_amount) =
                legacy_spending.unwrap_or((env.ledger().timestamp(), 0));

            // The legacy `UserVolume` already counts every amount in the
            // current window (each payment incremented both counters), so the
            // window balance must not be added again here.
            env.storage().persistent().set(
                &record_key,
                &pack_user_record(&env, last_reset_time, accumulated_amount, legacy_volume),
            );
            env.storage().persistent().extend_ttl(
                &record_key,
                Self::PERSISTENT_LIFETIME_THRESHOLD,
                Self::PERSISTENT_BUMP_AMOUNT,
            );
        }

        env.storage()
            .persistent()
            .remove(&DataKey::UserSpending(user.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::UserVolume(user.clone()));

        env.events()
            .publish((symbol_short!("migrated"), user), legacy_volume);

        true
    }

    /// Adds an address to the blacklist. ComplianceOfficer-protected.
    ///
    /// # Parameters
    /// - `address`: Address to blacklist; subsequent payments to it as a
    ///   recipient will be rejected.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the current ComplianceOfficer does not authorize the call.
    pub fn blacklist_address(env: Env, address: Address) -> Result<(), Error> {
        Self::require_role(&env, Role::ComplianceOfficer)?;

        env.storage()
            .persistent()
            .set(&DataKey::Blacklist(address.clone()), &true);
        env.storage().persistent().extend_ttl(
            &DataKey::Blacklist(address),
            Self::PERSISTENT_LIFETIME_THRESHOLD,
            Self::PERSISTENT_BUMP_AMOUNT,
        );

        Ok(())
    }

    /// Removes an address from the blacklist. ComplianceOfficer-protected.
    ///
    /// # Parameters
    /// - `address`: Address to remove from the blacklist.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the current ComplianceOfficer does not authorize the call.
    pub fn unblacklist_address(env: Env, address: Address) -> Result<(), Error> {
        Self::require_role(&env, Role::ComplianceOfficer)?;

        env.storage()
            .persistent()
            .remove(&DataKey::Blacklist(address));

        Ok(())
    }

    /// Returns whether an address is blacklisted.
    ///
    /// # Parameters
    /// - `address`: Address to check.
    ///
    /// # Returns
    /// `true` if `address` is blacklisted, `false` otherwise.
    ///
    /// # Panics
    /// Does not panic.
    pub fn is_blacklisted(env: Env, address: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Blacklist(address))
            .unwrap_or(false)
    }

    /// Returns the effective fee_bps for a sender after applying any
    /// volume-based tiered discount.
    ///
    /// # Parameters
    /// - `sender`: Address whose discounted fee rate to compute.
    ///
    /// # Returns
    /// The configured `fee_bps`, halved if `sender`'s lifetime volume
    /// exceeds the tiered-discount threshold, or `0` if not initialized.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_effective_fee_bps(env: Env, sender: Address) -> i128 {
        let fee_bps: i128 = env.storage().instance().get(&DataKey::FeeBps).unwrap_or(0);
        let user_volume = Self::get_user_volume(env.clone(), sender);
        if user_volume > Self::VOLUME_THRESHOLD {
            fee_bps / 2
        } else {
            fee_bps
        }
    }

    /// Set a new admin. SuperAdmin-protected.
    ///
    /// # Parameters
    /// - `new_admin`: Address to install as the new admin.
    ///
    /// # Returns
    /// Always `Ok(())`.
    ///
    /// # Panics
    /// Panics if an admin is already set and current SuperAdmin does not authorize the call.
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            Self::require_role(&env, Role::SuperAdmin)?;
        }
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        Self::set_role_internal(&env, Role::SuperAdmin, &new_admin);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );
        Ok(())
    }

    /// Transfers admin rights to a new address. Requires current SuperAdmin authorization.
    ///
    /// # Parameters
    /// - `new_admin`: Address to become the new admin.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the current SuperAdmin does not authorize the call.
    ///
    /// DEPRECATED for direct use.  Queue via `queue_action(ActionType::TransferAdmin(…))`.
    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        let current_admin = Self::require_role(&env, Role::SuperAdmin)?;
        Self::remove_role_internal(&env, Role::SuperAdmin, &current_admin);
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        Self::set_role_internal(&env, Role::SuperAdmin, &new_admin);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );
        Ok(())
    }

    /// Arms the dead man's switch directly (bypassing the timelock).
    ///
    /// # Parameters
    /// - `backup`: Address allowed to claim admin rights after the timeout.
    /// - `timeout_seconds`: Inactivity window in seconds. Must be at least
    ///   `MIN_DMS_TIMEOUT` (7 days).
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet, or `Err(Error::InvalidDmsConfig)` if
    /// `timeout_seconds` is below the minimum or `backup` equals the
    /// current admin.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call.
    pub fn set_backup_admin_internal(
        env: Env,
        backup: Address,
        timeout_seconds: u64,
    ) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();
        Self::apply_set_backup_admin(&env, backup, timeout_seconds)
    }

    /// Shared implementation for arming the dead man's switch: validates the
    /// configuration, stores the backup admin and timeout, and starts the
    /// claim clock. Performs NO authorization; callers must do that first
    /// (direct admin call or timelock execution).
    pub(crate) fn apply_set_backup_admin(
        env: &Env,
        backup: Address,
        timeout_seconds: u64,
    ) -> Result<(), Error> {
        if timeout_seconds < Self::MIN_DMS_TIMEOUT {
            return Err(Error::InvalidDmsConfig);
        }
        let admin = Self::require_admin(env)?;
        if backup == admin {
            return Err(Error::InvalidDmsConfig);
        }

        env.storage().instance().set(&DataKey::BackupAdmin, &backup);
        env.storage()
            .instance()
            .set(&DataKey::DmsTimeout, &timeout_seconds);
        // The claim clock starts ticking from the moment the switch is armed.
        env.storage()
            .instance()
            .set(&DataKey::LastHeartbeat, &env.ledger().timestamp());
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events().publish(
            (Symbol::new(env, "backup_admin_set"), admin),
            (backup, timeout_seconds),
        );

        log!(env, "Dead man's switch armed");
        Ok(())
    }

    /// Disables the dead man's switch, removing the backup admin and its
    /// claim window. The inactivity timeout is left in storage untouched so
    /// a later re-arming (via timelock) can reuse it.
    ///
    /// Admin authorization is required.
    pub fn remove_backup_admin(env: Env) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        env.storage().instance().remove(&DataKey::BackupAdmin);
        env.storage().instance().remove(&DataKey::LastHeartbeat);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events().publish(
            (Symbol::new(&env, "backup_admin_removed"), admin),
            env.ledger().timestamp(),
        );

        log!(&env, "Dead man's switch disarmed");
        Ok(())
    }

    // ── Dead-man's switch operator heartbeat renewal (issue #889) ────────────

    /// Appoints `operator` as the address authorised to renew the
    /// dead-man's switch heartbeat without full admin rights.
    ///
    /// This keeps the primary admin key in cold storage during routine monthly
    /// heartbeat cycles. The operator can *only* reset the
    /// `LastHeartbeat` timestamp; it cannot change the backup admin address
    /// or the grace period.
    ///
    /// Admin authorization is required.
    ///
    /// # Parameters
    /// - `operator`: Address to designate as the heartbeat operator.
    ///   Must not be the current admin.
    ///
    /// # Errors
    /// - [`Error::NotInitialized`] — contract not yet initialized.
    /// - [`Error::InvalidDmsConfig`] — `operator` is the current admin.
    pub fn set_heartbeat_operator(env: Env, operator: Address) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();
        dead_mans_switch::set_heartbeat_operator(&env, &admin, &operator)
    }

    /// Resets the dead-man's switch heartbeat clock to the current ledger
    /// timestamp. The caller must be the designated heartbeat operator.
    ///
    /// This is the routine call an operator makes to keep the switch alive
    /// without ever touching the primary admin key.
    ///
    /// # Parameters
    /// - `operator`: The calling operator address. Must match the stored
    ///   [`DataKey::HeartbeatOperator`] and must authorize the call.
    ///
    /// # Errors
    /// - [`Error::Unauthorized`] — no operator configured or caller is not
    ///   the designated operator.
    pub fn renew_heartbeat(env: Env, operator: Address) -> Result<(), Error> {
        dead_mans_switch::renew_heartbeat(&env, &operator)
    }

    /// Returns the currently configured heartbeat operator, if any.
    ///
    /// # Returns
    /// `Some(operator)` if one has been appointed, else `None`.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_heartbeat_operator(env: Env) -> Option<Address> {
        dead_mans_switch::get_heartbeat_operator(&env)
    }

    /// Configures the lending protocol used for treasury yield operations. TreasuryManager-protected.
    pub fn set_yield_protocol(env: Env, protocol: Address) -> Result<(), Error> {
        Self::require_role(&env, Role::TreasuryManager)?;

        env.storage()
            .instance()
            .set(&DataKey::YieldProtocol, &protocol);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );
        env.events()
            .publish((symbol_short!("yield_cfg"),), protocol);
        Ok(())
    }

    /// Configures the trusted KYC oracle and the high-value payment threshold. ComplianceOfficer-protected.
    pub fn set_kyc_config(env: Env, oracle: Address, threshold: i128) -> Result<(), Error> {
        if threshold < 0 {
            return Err(Error::InvalidKycThreshold);
        }
        Self::require_role(&env, Role::ComplianceOfficer)?;

        env.storage().instance().set(&DataKey::KycOracle, &oracle);
        env.storage()
            .instance()
            .set(&DataKey::KycThreshold, &threshold);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );
        env.events()
            .publish((symbol_short!("kyc_cfg"), oracle), threshold);
        Ok(())
    }

    /// Deposits idle treasury funds into the configured lending protocol.
    ///
    /// Both the TreasuryManager and treasury authorize this operation. The second
    /// authorization is required because the funds are held by the treasury,
    /// rather than by this router contract.
    pub fn deposit_to_yield(env: Env, token: Address, amount: i128) -> Result<(), Error> {
        // Circuit breaker: moving treasury funds into yield is non-essential.
        Self::require_circuit_closed(&env)?;
        if amount <= 0 {
            return Err(Error::InvalidYieldAmount);
        }

        Self::require_role(&env, Role::TreasuryManager)?;
        let treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::PlatformTreasury)
            .ok_or(Error::NotInitialized)?;
        treasury.require_auth();
        let protocol: Address = env
            .storage()
            .instance()
            .get(&DataKey::YieldProtocol)
            .ok_or(Error::YieldProtocolNotConfigured)?;

        LendingProtocolClient::new(&env, &protocol).deposit(&treasury, &token, &amount);

        let key = DataKey::YieldPrincipal(token.clone());
        let principal: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(principal + amount));
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_LIFETIME_THRESHOLD,
            Self::PERSISTENT_BUMP_AMOUNT,
        );
        env.events()
            .publish((symbol_short!("yield_dep"), token), amount);
        Ok(())
    }

    /// Withdraws treasury principal from the configured lending protocol. TreasuryManager-protected.
    pub fn withdraw_from_yield(env: Env, token: Address, amount: i128) -> Result<(), Error> {
        // Circuit breaker: yield principal movements are non-essential while
        // paused; `emergency_withdraw` remains the funds-out path.
        Self::require_circuit_closed(&env)?;
        let key = DataKey::YieldPrincipal(token.clone());
        let principal: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount <= 0 || amount > principal {
            return Err(Error::InvalidYieldAmount);
        }

        Self::require_role(&env, Role::TreasuryManager)?;
        let treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::PlatformTreasury)
            .ok_or(Error::NotInitialized)?;
        let protocol: Address = env
            .storage()
            .instance()
            .get(&DataKey::YieldProtocol)
            .ok_or(Error::YieldProtocolNotConfigured)?;

        LendingProtocolClient::new(&env, &protocol).withdraw(&treasury, &token, &amount);
        let remaining = principal - amount;
        if remaining == 0 {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &remaining);
            env.storage().persistent().extend_ttl(
                &key,
                Self::PERSISTENT_LIFETIME_THRESHOLD,
                Self::PERSISTENT_BUMP_AMOUNT,
            );
        }
        env.events()
            .publish((symbol_short!("yield_wdr"), token), amount);
        Ok(())
    }

    /// Claims all currently available yield to the platform treasury. TreasuryManager-protected.
    pub fn harvest_yield(env: Env, token: Address) -> Result<i128, Error> {
        // Circuit breaker: yield harvesting is a non-essential state change.
        Self::require_circuit_closed(&env)?;
        Self::require_role(&env, Role::TreasuryManager)?;
        let treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::PlatformTreasury)
            .ok_or(Error::NotInitialized)?;
        let protocol: Address = env
            .storage()
            .instance()
            .get(&DataKey::YieldProtocol)
            .ok_or(Error::YieldProtocolNotConfigured)?;

        let harvested = LendingProtocolClient::new(&env, &protocol).harvest(&treasury, &token);
        env.events()
            .publish((symbol_short!("yield_har"), token), harvested);
        Ok(harvested)
    }

    /// Returns the tracked principal deposited for `token`.
    pub fn get_yield_position(env: Env, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::YieldPrincipal(token))
            .unwrap_or(0)
    }

    /// Returns the configured KYC threshold, or `None` when enforcement is off.
    pub fn get_kyc_threshold(env: Env) -> Option<i128> {
        env.storage().instance().get(&DataKey::KycThreshold)
    }

    // ── Price-feed oracle ────────────────────────────────────────────────────

    /// Configures the price-feed oracle contract address. ComplianceOfficer-protected.
    ///
    /// The oracle contract must implement the [`PriceFeedOracle`] interface:
    /// it must expose a `get_price(base_asset, quote_asset) -> PriceData`
    /// method that returns the latest price together with a Unix timestamp so
    /// staleness can be validated against the configured threshold.
    ///
    /// # Parameters
    /// - `oracle`: Address of the oracle contract to use for price lookups.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has not been initialized.
    ///
    /// # Panics
    /// Panics if the current ComplianceOfficer does not authorize the call.
    pub fn set_price_oracle(env: Env, oracle: Address) -> Result<(), Error> {
        Self::require_role(&env, Role::ComplianceOfficer)?;

        env.storage()
            .instance()
            .set(&DataKey::OracleAddress, &oracle);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events().publish((symbol_short!("price_cfg"),), oracle);
        Ok(())
    }

    /// Sets the maximum age (in seconds) a price reading may have before it is
    /// considered stale. ComplianceOfficer-protected.
    ///
    /// When a price timestamp is older than `(current_ledger_time - threshold)`
    /// the reading is rejected with [`Error::OraclePriceStale`] and the
    /// fallback price (if configured) is used instead.
    ///
    /// # Parameters
    /// - `threshold_secs`: Maximum allowed age in seconds. A value of `0`
    ///   disables the staleness check entirely (every price is accepted).
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Panics
    /// Panics if the current ComplianceOfficer does not authorize the call.
    pub fn set_staleness_threshold(env: Env, threshold_secs: u64) -> Result<(), Error> {
        Self::require_role(&env, Role::ComplianceOfficer)?;

        env.storage()
            .instance()
            .set(&DataKey::StalenessThreshold, &threshold_secs);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events()
            .publish((symbol_short!("stale_cfg"),), threshold_secs);
        Ok(())
    }

    /// Stores an admin-supplied fallback price for a (base, quote) asset pair.
    /// ComplianceOfficer-protected.
    ///
    /// The fallback is used by [`get_price`] when the live oracle is
    /// unavailable or returns data that fails validation (stale or invalid).
    /// Setting a fallback price to `0` effectively removes the fallback,
    /// meaning that oracle failures will propagate as errors rather than
    /// silently using a stale cached value.
    ///
    /// # Parameters
    /// - `base_asset`: Address of the base asset (e.g. XLM contract).
    /// - `quote_asset`: Address of the quote asset (e.g. USDC contract).
    /// - `fallback_price`: Price expressed in the same fixed-point format as
    ///   the oracle (`price / 10^decimals`). Pass `0` to clear the fallback.
    /// - `decimals`: Decimal precision of `fallback_price`.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Panics
    /// Panics if the current ComplianceOfficer does not authorize the call.
    pub fn set_fallback_price(
        env: Env,
        base_asset: Address,
        quote_asset: Address,
        fallback_price: i128,
        decimals: u32,
    ) -> Result<(), Error> {
        Self::require_role(&env, Role::ComplianceOfficer)?;

        let key = DataKey::FallbackPrice(base_asset.clone(), quote_asset.clone());
        if fallback_price == 0 {
            // A zero fallback means "no fallback configured": remove the entry.
            env.storage().persistent().remove(&key);
        } else {
            let data = PriceData {
                price: fallback_price,
                decimals,
                // Timestamp 0 signals "static fallback — staleness does not apply".
                timestamp: 0,
            };
            env.storage().persistent().set(&key, &data);
            env.storage().persistent().extend_ttl(
                &key,
                Self::PERSISTENT_LIFETIME_THRESHOLD,
                Self::PERSISTENT_BUMP_AMOUNT,
            );
        }

        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events().publish(
            (symbol_short!("fall_cfg"), base_asset, quote_asset),
            fallback_price,
        );
        Ok(())
    }

    /// Returns the stored fallback price for a (base, quote) asset pair, if any.
    ///
    /// # Parameters
    /// - `base_asset`: Address of the base asset.
    /// - `quote_asset`: Address of the quote asset.
    ///
    /// # Returns
    /// `Some(PriceData)` if a fallback has been configured, `None` otherwise.
    pub fn get_fallback_price(
        env: Env,
        base_asset: Address,
        quote_asset: Address,
    ) -> Option<PriceData> {
        env.storage()
            .persistent()
            .get(&DataKey::FallbackPrice(base_asset, quote_asset))
    }

    /// Fetches the current exchange rate for a `(base_asset, quote_asset)`
    /// pair from the configured price-feed oracle, validates it, and returns
    /// the result.
    ///
    /// Every failure path below first attempts to serve an admin-configured
    /// fallback price for the pair; the oracle error is only surfaced when no
    /// fallback exists.
    ///
    /// ## Validation flow
    ///
    /// 1. **Oracle configured?** - Otherwise `Err(Error::OracleNotConfigured)`.
    /// 2. **Call oracle** - Invoke the oracle's `get_price`; a trapped or
    ///    unavailable contract yields `Err(Error::OracleCallFailed)`.
    /// 3. **Staleness check** - Reject a `price_data.timestamp` older than the
    ///    configured threshold (default 3 600 s) with
    ///    `Err(Error::OraclePriceStale)`. A threshold of `0` disables this check.
    /// 4. **Validity check** - A price <= 0 is invalid
    ///    (`Err(Error::OraclePriceInvalid)`).
    /// 5. **Return** - The validated `PriceData` is returned to the caller.
    ///
    /// `base_asset` is typically the XLM native contract and `quote_asset` the
    /// USDC contract.
    pub fn get_price(
        env: Env,
        base_asset: Address,
        quote_asset: Address,
    ) -> Result<PriceData, Error> {
        // Retrieve the oracle address, falling back gracefully if absent.
        let oracle_opt: Option<Address> = env.storage().instance().get(&DataKey::OracleAddress);

        let staleness_threshold: u64 = env
            .storage()
            .instance()
            .get(&DataKey::StalenessThreshold)
            .unwrap_or(3_600u64); // default: 1 hour

        // Helper closure: return the fallback price if one is configured,
        // otherwise propagate the supplied error.
        let fallback_or_err =
            |env: &Env, base: &Address, quote: &Address, err: Error| -> Result<PriceData, Error> {
                if let Some(fallback) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, PriceData>(&DataKey::FallbackPrice(base.clone(), quote.clone()))
                {
                    log!(env, "Oracle error; using fallback price");
                    Ok(fallback)
                } else {
                    Err(err)
                }
            };

        // 1. Check oracle is configured.
        let Some(oracle) = oracle_opt else {
            return fallback_or_err(&env, &base_asset, &quote_asset, Error::OracleNotConfigured);
        };

        // 2. Call the oracle. Use try_get_price to avoid trapping on failure.
        let Ok(Ok(price_data)) =
            PriceFeedOracleClient::new(&env, &oracle).try_get_price(&base_asset, &quote_asset)
        else {
            log!(&env, "Oracle contract call failed");
            return fallback_or_err(&env, &base_asset, &quote_asset, Error::OracleCallFailed);
        };

        // 3. Staleness check (skip when threshold is 0).
        if staleness_threshold > 0 {
            let current_time = env.ledger().timestamp();
            if price_data.timestamp == 0
                || current_time.saturating_sub(price_data.timestamp) > staleness_threshold
            {
                log!(&env, "Oracle price is stale");
                return fallback_or_err(&env, &base_asset, &quote_asset, Error::OraclePriceStale);
            }
        }

        // 4. Validity check.
        if price_data.price <= 0 {
            log!(&env, "Oracle price is invalid (<=0)");
            return fallback_or_err(&env, &base_asset, &quote_asset, Error::OraclePriceInvalid);
        }

        // 5. Emit event and return the validated price.
        env.events().publish(
            (
                symbol_short!("price_ok"),
                base_asset.clone(),
                quote_asset.clone(),
            ),
            price_data.price,
        );

        log!(&env, "Oracle price fetched and validated");
        Ok(price_data)
    }

    /// Routes a payment from a sender to a recipient, deducting a platform fee.
    ///
    /// # Parameters
    /// - `sender`: Address the funds are debited from; must authorize the call.
    /// - `recipient`: Address to receive the funds (minus the platform fee).
    /// - `token_address`: Contract ID of the token being transferred.
    /// - `amount`: Amount to route, in the token's smallest unit. Must be
    ///   positive and within the configured min/max and daily-limit bounds.
    ///
    /// This is the primary way for the admin to reset the timer without
    /// changing any contract state. Routing a payment also bumps the
    /// heartbeat automatically. A no-op when no backup admin is configured.
    ///
    /// Admin authorization is required. Works even while the contract is
    /// frozen so the legitimate admin can keep the switch alive.
    pub fn ping(env: Env) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        Self::bump_heartbeat(&env);

        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events()
            .publish((symbol_short!("ping"), admin), env.ledger().timestamp());

        log!(&env, "Admin heartbeat recorded");
        Ok(())
    }

    /// Dead man's switch claim: transfers admin rights to `claimant` if they
    /// are the configured backup admin and the admin has not pinged (directly
    /// or via routing a payment) for at least the configured timeout.
    ///
    /// Deliberately NOT timelocked and NOT blocked by a contract freeze: if
    /// the primary admin is gone there is nobody left to execute a queued
    /// action, and freezing must not be able to brick recovery.
    ///
    /// # Parameters
    /// - `claimant`: Address claiming admin rights. Must be the configured
    ///   backup admin and must authorize the call.
    ///
    /// # Returns
    /// `Ok(())` on success, `Err(Error::NoBackupAdmin)` if no backup is
    /// configured, `Err(Error::Unauthorized)` if `claimant` is not the
    /// backup, or `Err(Error::DmsNotReady)` if the timeout has not elapsed.
    pub fn claim_admin(env: Env, claimant: Address) -> Result<(), Error> {
        claimant.require_auth();

        let backup: Address = env
            .storage()
            .instance()
            .get(&DataKey::BackupAdmin)
            .ok_or(Error::NoBackupAdmin)?;

        if claimant != backup {
            return Err(Error::Unauthorized);
        }

        let timeout: u64 = env
            .storage()
            .instance()
            .get(&DataKey::DmsTimeout)
            .ok_or(Error::InvalidDmsConfig)?;
        let last_heartbeat: u64 = env
            .storage()
            .instance()
            .get(&DataKey::LastHeartbeat)
            .ok_or(Error::InvalidDmsConfig)?;

        let now = env.ledger().timestamp();
        if now < last_heartbeat.saturating_add(timeout) {
            return Err(Error::DmsNotReady);
        }

        env.storage().instance().set(&DataKey::Admin, &claimant);
        // Fully disarm the switch: the backup is consumed and the clock is
        // cleared so the new admin starts from a clean slate (re-arm via the
        // timelock if desired).
        env.storage().instance().remove(&DataKey::BackupAdmin);
        env.storage().instance().remove(&DataKey::LastHeartbeat);
        env.storage().instance().remove(&DataKey::DmsTimeout);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        env.events()
            .publish((Symbol::new(&env, "admin_claimed"), claimant.clone()), now);

        log!(&env, "Admin claimed via dead man's switch");
        Ok(())
    }

    /// Returns the current admin address.
    ///
    /// # Returns
    /// `Some(admin)` if the contract is initialized, otherwise `None`.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Admin)
    }

    /// Returns the dead man's switch configuration.
    ///
    /// # Returns
    /// `(backup_admin, timeout_seconds, last_heartbeat, seconds_since_heartbeat)`.
    /// `backup_admin` is `None` and the timestamps `0` when no backup is
    /// configured.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_dead_mans_switch(env: Env) -> (Option<Address>, u64, u64, u64) {
        let backup = env.storage().instance().get(&DataKey::BackupAdmin);
        let timeout: u64 = env
            .storage()
            .instance()
            .get(&DataKey::DmsTimeout)
            .unwrap_or(0);
        let last: u64 = env
            .storage()
            .instance()
            .get(&DataKey::LastHeartbeat)
            .unwrap_or(0);
        let now = env.ledger().timestamp();
        let elapsed = if backup.is_some() {
            now.saturating_sub(last)
        } else {
            0
        };
        (backup, timeout, last, elapsed)
    }

    /// Returns `true` if the dead man's switch is armed (a backup admin is
    /// configured) and the timeout has elapsed, i.e. `claim_admin` would
    /// currently succeed for the backup.
    ///
    /// # Panics
    /// Does not panic.
    pub fn is_dead_mans_switch_expired(env: Env) -> bool {
        let backup_set = env.storage().instance().has(&DataKey::BackupAdmin);
        if !backup_set {
            return false;
        }
        let timeout: u64 = env
            .storage()
            .instance()
            .get(&DataKey::DmsTimeout)
            .unwrap_or(0);
        let last: u64 = env
            .storage()
            .instance()
            .get(&DataKey::LastHeartbeat)
            .unwrap_or(0);
        env.ledger().timestamp().saturating_sub(last) >= timeout
    }

    /// Recovers tokens accidentally sent directly to the contract address. Admin-only.
    ///
    /// # Parameters
    /// - `token`: Contract ID of the token to recover.
    /// - `amount`: Amount to transfer from the contract's balance to the admin.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call, or if the
    /// token transfer fails (e.g. the contract's balance is below `amount`).
    pub fn recover_tokens(env: Env, token: Address, amount: i128) -> Result<(), Error> {
        // Circuit breaker: token recovery is a non-essential state change (the
        // TreasuryManager still has `emergency_withdraw` while paused).
        Self::require_circuit_closed(&env)?;
        let treasury_mgr = Self::require_role(&env, Role::TreasuryManager)?;

        let contract_address = env.current_contract_address();
        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&contract_address, &treasury_mgr, &amount);

        Ok(())
    }

    /// Records a token as supported (no-op; routing accepts any token contract ID).
    ///
    /// # Parameters
    /// - `_token`: Ignored; present for API compatibility.
    ///
    /// # Returns
    /// Always `Ok(())`.
    ///
    /// # Panics
    /// Does not panic.
    pub fn add_supported_token(_env: Env, _token: Address) -> Result<(), Error> {
        Ok(())
    }

    /// Routes a payment from a sender to a recipient, deducting a platform fee.
    ///
    /// # Parameters
    /// - `sender`: Address funds are debited from; must authorize call.
    /// - `recipient`: Address to receive funds (minus platform fee).
    /// - `token_address`: Contract ID of token transferred.
    /// - `amount`: Amount to route, in token's smallest unit.
    ///
    /// # Returns
    /// `Ok(())` on success, or appropriate `Error` on failure.
    pub fn route_payment(
        env: Env,
        sender: Address,
        recipient: Address,
        token_address: Address,
        amount: i128,
    ) -> Result<(), Error> {
        let _guard = ReentrancyGuard::new(&env)?;
        if Self::is_frozen_internal(&env) {
            return Err(Error::ContractFrozen);
        }
        // #890: direct payments are blocked only under FullHalt, not SwapsOnly.
        Self::require_not_full_halt(&env)?;

        let max_amount: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MaxAmount)
            .unwrap_or(Self::MAX_AMOUNT);
        let min_limit: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MinLimit)
            .unwrap_or(0);

        // Validate everything that can be checked without auth first: a
        // `require_auth` abort rolls back the whole invocation, so rejecting
        // bad input before authenticating avoids turning ordinary validation
        // errors into an opaque host rollback panic.
        if sender == recipient {
            return Err(Error::InvalidRecipient);
        }
        if Self::is_blacklisted(env.clone(), recipient.clone()) {
            return Err(Error::Blacklisted);
        }
        if amount <= 0 || amount > max_amount {
            return Err(Error::LimitExceeded);
        }
        if amount < min_limit {
            return Err(Error::LimitExceeded);
        }
        Self::verify_kyc_for_amount(&env, &sender, amount)?;

        sender.require_auth();

        // Contract-level rate limiting (issue #716): one invocation slot per
        // payment, consumed after auth so only authorized senders are counted.
        Self::check_rate_limit(&env, &sender)?;

        let (platform_treasury, fee_bps, fee_cap) = Self::load_fee_config(&env)?;

        Self::process_single_payment(
            &env,
            &sender,
            &recipient,
            &token_address,
            amount,
            &platform_treasury,
            fee_bps,
            fee_cap,
        )?;

        // A successful routing proves the platform is alive and serving, so
        // bump the dead man's switch heartbeat (no-op without a backup admin).
        Self::bump_heartbeat(&env);

        Ok(())
    }

    /// Swaps `token_in` through a caller-supplied DEX path and routes the
    /// resulting `token_out` to the recipient. The DEX adapter must return the
    /// received output and any unused input as `[received, unused]`; unused
    /// input is credited to the sender's refund balance.
    ///
    /// # Panics
    /// Panics if the DEX router returns a swap result with fewer than 2 elements,
    /// or if `swap_result.get(0)` or `swap_result.get(1)` returns `None`.
    #[allow(clippy::too_many_arguments)]
    pub fn route_payment_with_swap_raw(
        env: Env,
        sender: Address,
        recipient: Address,
        dex_router: Address,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
        path: Vec<Address>,
        min_amount_out: i128,
    ) -> Result<(), Error> {
        if Self::is_frozen_internal(&env) {
            return Err(Error::ContractFrozen);
        }
        // #890: swap routing is blocked under SwapsOnly AND FullHalt.
        Self::require_swaps_unpaused(&env)?;
        if sender == recipient {
            return Err(Error::InvalidRecipient);
        }
        if Self::is_blacklisted(env.clone(), recipient.clone()) {
            return Err(Error::Blacklisted);
        }
        Self::validate_swap_path(&token_in, &token_out, &path, min_amount_out)?;

        let max_amount: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MaxAmount)
            .unwrap_or(Self::MAX_AMOUNT);
        let min_limit: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MinLimit)
            .unwrap_or(0);
        if amount_in <= 0 || amount_in > max_amount || amount_in < min_limit {
            return Err(Error::LimitExceeded);
        }
        Self::verify_kyc_for_amount(&env, &sender, amount_in)?;
        sender.require_auth();

        let (platform_treasury, fee_bps, fee_cap) = Self::load_fee_config(&env)?;
        let token_in_client = token::Client::new(&env, &token_in);
        token_in_client.transfer(&sender, &dex_router, &amount_in);

        let swap_result = DexRouterClient::new(&env, &dex_router).swap_exact_tokens_for_tokens(
            &token_in,
            &token_out,
            &amount_in,
            &min_amount_out,
            &path,
            &env.current_contract_address(),
        );
        if swap_result.len() != 2 {
            return Err(Error::InvalidSwapPath);
        }
        let amount_received: i128 = swap_result.get(0).unwrap();
        let unused_input: i128 = swap_result.get(1).unwrap();
        if amount_received < min_amount_out || amount_received <= 0 || unused_input < 0 {
            return Err(Error::SlippageExceeded);
        }
        if unused_input > 0 {
            token_in_client.transfer(&env.current_contract_address(), &sender, &unused_input);
        }

        Self::process_single_payment(
            &env,
            &sender,
            &recipient,
            &token_out,
            amount_received,
            &platform_treasury,
            fee_bps,
            fee_cap,
        )
    }

    /// Routes a payment through two to four consecutive DEX swaps. `path`
    /// contains the input asset followed by every intermediate asset and the
    /// output asset. `min_amounts_out[i]` is the required output for hop `i`.
    /// Any input dust returned by a hop is refunded to `sender` in that hop's
    /// input asset. All swaps and settlement are atomic.
    pub fn route_multihop_swap(
        env: Env,
        sender: Address,
        recipient: Address,
        dex_router: Address,
        amount_in: i128,
        path: Vec<Address>,
        min_amounts_out: Vec<i128>,
    ) -> Result<i128, Error> {
        let _guard = ReentrancyGuard::new(&env)?;
        if Self::is_frozen_internal(&env) {
            return Err(Error::ContractFrozen);
        }
        // #890: multihop swap routing is blocked under SwapsOnly AND FullHalt.
        Self::require_swaps_unpaused(&env)?;
        if sender == recipient {
            return Err(Error::InvalidRecipient);
        }
        if Self::is_blacklisted(env.clone(), recipient.clone()) {
            return Err(Error::Blacklisted);
        }

        let hop_count = path.len().saturating_sub(1);
        if !(2..=4).contains(&hop_count) || min_amounts_out.len() != hop_count {
            return Err(Error::InvalidSwapPath);
        }
        // Paths are intentionally short (at most five assets), so checking
        // every pair keeps the cycle guard simple and bounded.
        for i in 0..path.len() {
            let asset = path.get(i).ok_or(Error::InvalidSwapPath)?;
            for j in (i + 1)..path.len() {
                if path.get(j).as_ref() == Some(&asset) {
                    return Err(Error::CyclicSwapPath);
                }
            }
        }
        for floor in min_amounts_out.iter() {
            if floor <= 0 {
                return Err(Error::InvalidSwapParams);
            }
        }
        if !Self::is_dex_registered_internal(&env, &dex_router) {
            return Err(Error::DexNotRegistered);
        }

        let max_amount: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MaxAmount)
            .unwrap_or(Self::MAX_AMOUNT);
        let min_limit: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MinLimit)
            .unwrap_or(0);
        if amount_in <= 0 || amount_in > max_amount || amount_in < min_limit {
            return Err(Error::LimitExceeded);
        }
        Self::verify_kyc_for_amount(&env, &sender, amount_in)?;
        sender.require_auth();
        Self::check_rate_limit(&env, &sender)?;

        let (platform_treasury, fee_bps, fee_cap) = Self::load_fee_config(&env)?;
        let user_volume = Self::accrue_user_record(&env, &sender, amount_in)?;
        let effective_fee_bps = if user_volume > Self::VOLUME_THRESHOLD {
            fee_bps / 2
        } else {
            fee_bps
        };
        let contract_address = env.current_contract_address();
        let first_token = path.get(0).ok_or(Error::InvalidSwapPath)?;
        let input_client = token::Client::new(&env, &first_token);
        if input_client.balance(&sender) < amount_in {
            return Err(Error::InsufficientBalance);
        }
        input_client.transfer(&sender, &contract_address, &amount_in);

        let mut current_amount = amount_in;
        for i in 0..hop_count {
            let token_in = path.get(i).ok_or(Error::InvalidSwapPath)?;
            let token_out = path.get(i + 1).ok_or(Error::InvalidSwapPath)?;
            let floor = min_amounts_out.get(i).ok_or(Error::InvalidSwapPath)?;
            let token_in_client = token::Client::new(&env, &token_in);
            let token_out_client = token::Client::new(&env, &token_out);
            let input_before = token_in_client.balance(&contract_address);
            let output_before = token_out_client.balance(&contract_address);
            token_in_client.transfer(&contract_address, &dex_router, &current_amount);
            let hop_path = vec![&env, token_in.clone(), token_out.clone()];
            let result = DexRouterClient::new(&env, &dex_router).swap_exact_tokens_for_tokens(
                &token_in,
                &token_out,
                &current_amount,
                &floor,
                &hop_path,
                &contract_address,
            );
            if result.len() != 2 {
                return Err(Error::InvalidSwapPath);
            }
            let reported_out: i128 = result.get(0).ok_or(Error::SwapFailed)?;
            let reported_unused: i128 = result.get(1).ok_or(Error::SwapFailed)?;
            let input_after = token_in_client.balance(&contract_address);
            let output_after = token_out_client.balance(&contract_address);
            let actual_unused = input_after - (input_before - current_amount);
            let actual_out = output_after - output_before;
            if reported_unused < 0
                || reported_unused > current_amount
                || actual_unused != reported_unused
                || reported_out != actual_out
                || actual_out < floor
                || actual_out <= 0
            {
                return Err(Error::SlippageExceeded);
            }
            if actual_unused > 0 {
                // The path cannot revisit an asset, so this hop's unused input
                // is no longer needed by a later hop.
                token_in_client.transfer(&contract_address, &sender, &actual_unused);
            }
            env.events().publish(
                (
                    Symbol::new(&env, "swap_hop"),
                    dex_router.clone(),
                    token_in,
                    token_out,
                ),
                (current_amount, actual_out, actual_unused, floor),
            );
            current_amount = actual_out;
        }

        let final_token = path.get(path.len() - 1).ok_or(Error::InvalidSwapPath)?;
        let output_client = token::Client::new(&env, &final_token);
        let (fee_amount, remainder) =
            Self::calculate_fee(current_amount, effective_fee_bps, fee_cap);
        if fee_amount > 0 {
            output_client.transfer(&contract_address, &platform_treasury, &fee_amount);
        }
        if remainder > 0 {
            match output_client.try_transfer(&contract_address, &recipient, &remainder) {
                Ok(Ok(())) => {}
                _ => Self::credit_refund_balance(&env, &sender, &final_token, remainder),
            }
        }
        env.events().publish(
            (symbol_short!("routed"), sender.clone(), recipient.clone()),
            remainder,
        );
        Self::bump_heartbeat(&env);
        Ok(remainder)
    }

    /// Routes multiple payments in a single transaction. If any payment fails,
    /// the entire batch is reverted atomically.
    ///
    /// # Parameters
    /// - `payments`: Batch of transfer instructions to apply in order. See
    ///   [`Payment`] for per-item constraints.
    ///
    /// # Returns
    /// `Ok(())` if every payment in the batch succeeds. Returns the first
    /// error encountered (see `route_payment` for the possible `Err`
    /// variants and their causes) if any payment fails; the Soroban host
    /// reverts all storage and balance changes from the batch in that case.
    ///
    /// # Panics
    /// Panics if any payment's `sender` does not authorize the call, or if
    /// a token transfer to `platform_treasury` fails.
    pub fn route_payments(env: Env, payments: Vec<Payment>) -> Result<(), Error> {
        let _guard = ReentrancyGuard::new(&env)?;
        if Self::is_frozen_internal(&env) {
            return Err(Error::ContractFrozen);
        }
        // #890: batch direct payments are blocked only under FullHalt.
        Self::require_not_full_halt(&env)?;

        // Pre-validate the whole batch before authorizing or moving any
        // funds, so a rejected payment never triggers an auth rollback.
        let max_amount: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MaxAmount)
            .unwrap_or(Self::MAX_AMOUNT);
        let min_limit: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MinLimit)
            .unwrap_or(0);

        // Collect unique senders to require auth only once per sender.
        let mut seen_senders = Vec::new(&env);
        for payment in payments.iter() {
            if payment.sender == payment.recipient {
                return Err(Error::InvalidRecipient);
            }
            if Self::is_blacklisted(env.clone(), payment.recipient.clone()) {
                return Err(Error::Blacklisted);
            }
            if payment.amount <= 0 || payment.amount > max_amount {
                return Err(Error::LimitExceeded);
            }
            if payment.amount < min_limit {
                return Err(Error::LimitExceeded);
            }
            Self::verify_kyc_for_amount(&env, &payment.sender, payment.amount)?;
            // Contract-level rate limiting (issue #716): one invocation slot
            // per payment, so a batch costs the sender one slot per item.
            // Charged here, after auth, so only authorized senders are counted.
            // A later failure reverts the whole batch and refunds every slot.
            Self::check_rate_limit(&env, &payment.sender)?;

            // Track unique senders for auth
            let mut is_new = true;
            for seen in seen_senders.iter() {
                if seen == payment.sender {
                    is_new = false;
                    break;
                }
            }
            if is_new {
                seen_senders.push_back(payment.sender.clone());
                payment.sender.require_auth();
            }
        }

        let (platform_treasury, fee_bps, fee_cap) = Self::load_fee_config(&env)?;

        // Successful routing proves the platform is alive and serving, so
        // bump the dead man's switch heartbeat (no-op without a backup admin).
        Self::bump_heartbeat(&env);

        for payment in payments.iter() {
            Self::process_single_payment(
                &env,
                &payment.sender,
                &payment.recipient,
                &payment.token_address,
                payment.amount,
                &platform_treasury,
                fee_bps,
                fee_cap,
            )?;
        }

        // A successful batch proves the platform is alive and serving, so
        // bump the dead man's switch heartbeat (no-op without a backup admin).
        Self::bump_heartbeat(&env);

        Ok(())
    }

    /// Returns the current meta-transaction nonce for a user.
    ///
    /// Relayers must use this nonce when building the signed payload.
    /// The nonce starts at `0` and increments after each successful
    /// `route_payment_meta`, preventing replay attacks.
    pub fn get_meta_nonce(env: Env, user: Address) -> u64 {
        Self::get_meta_nonce_internal(&env, &user)
    }

    /// Routes a payment authorised by an off-chain relayer's Ed25519 signature
    /// instead of the sender's on-chain authorization.
    ///
    /// # Parameters
    /// - `sender`: Address whose funds are routed.
    /// - `signer_pubkey`: Ed25519 public key that signed the payload.
    /// - `recipient`: Destination address.
    /// - `token_address`: Token contract ID.
    /// - `amount`: Amount to route.
    /// - `nonce`: Sender's current meta-transaction nonce.
    /// - `deadline`: Expiration timestamp.
    /// - `signature`: Ed25519 signature over canonical payload.
    ///
    /// # Returns
    /// `Ok(())` once the payment has settled.
    #[allow(clippy::too_many_arguments)]
    pub fn route_payment_meta(
        env: Env,
        sender: Address,
        signer_pubkey: BytesN<32>,
        recipient: Address,
        token_address: Address,
        amount: i128,
        nonce: u64,
        deadline: u64,
        signature: BytesN<64>,
    ) -> Result<(), Error> {
        let _guard = ReentrancyGuard::new(&env)?;
        if Self::is_frozen_internal(&env) {
            return Err(Error::ContractFrozen);
        }
        if Self::is_paused(env.clone()) {
            return Err(Error::Paused);
        }

        let (platform_treasury, fee_bps, fee_cap) = Self::load_fee_config(&env)?;

        if env.ledger().timestamp() > deadline {
            return Err(Error::DeadlineExpired);
        }

        let stored = Self::get_meta_nonce_internal(&env, &sender);
        if stored != nonce {
            return Err(Error::InvalidNonce);
        }

        let message = Self::build_meta_message(
            &env,
            &sender,
            &signer_pubkey,
            &recipient,
            &token_address,
            amount,
            nonce,
            deadline,
        );
        // Traps on invalid signature; `Error::InvalidSignature` documents
        // this failure mode for off-chain integrators.
        env.crypto()
            .ed25519_verify(&signer_pubkey, &message, &signature);

        let key = DataKey::MetaNonce(sender.clone());
        env.storage().persistent().set(&key, &(nonce + 1));
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_LIFETIME_THRESHOLD,
            Self::PERSISTENT_BUMP_AMOUNT,
        );

        Self::process_single_payment_no_auth(
            &env,
            &sender,
            &recipient,
            &token_address,
            amount,
            &platform_treasury,
            fee_bps,
            fee_cap,
        )
    }

    // ── Token swaps: cross-contract DEX routing ──────────────────────────────
    //
    // A sender can settle a payment in any token they hold and have it
    // converted into the merchant's preferred token before delivery.  The
    // whole sequence runs inside one Soroban transaction:
    //
    //   sender ──sell_token──> router ──sell_token──> DEX adapter
    //   router <──buy_token────────────────────────────┘
    //     ├──fee──> platform treasury
    //     └───────> recipient
    //
    // Atomicity: any failure — DEX revert, slippage breach, deadline expiry,
    // transfer failure — returns an `Err`, and the Soroban host reverts every
    // balance and storage change the payment had already made.  The sender is
    // never left having paid a fee, and the recipient never sees a partial
    // payment.
    //
    // The swap itself is a cross-contract call into a registered DEX adapter,
    // which must expose:
    //
    //   swap(sell_token, buy_token, amount_in, min_amount_out, recipient) -> i128
    //   quote(sell_token, buy_token, amount_in) -> i128
    //
    // By the time `swap` runs, the router has already transferred `amount_in`
    // of `sell_token` to the adapter, so the adapter only has to sell it on and
    // send the `buy_token` it buys to `recipient`, which is this contract.
    // Because the input moves on this side of the call, the adapter never
    // needs authority over the router's balance.  Fixing the signature here,
    // rather than forwarding an opaque payload, also keeps this contract
    // independent of any one DEX's argument layout: an adapter can wrap
    // Soroswap, or any other router, behind this interface.  Only DEXes the
    // admin registered through the timelock may be called, so the call cannot
    // be pointed at arbitrary code.
    //
    // Two slippage guards protect the sender, and both are enforced against
    // the `buy_token` balance delta the router actually receives rather than
    // against whatever the DEX reports:
    //   * `min_amount_out` — a hard floor on the output;
    //   * `max_slippage_bps` — a ceiling on how far the output may fall below
    //     the caller's own quote (`expected_amount_out`).

    /// Core swap-routed payment logic shared by `route_payment_with_swap` and
    /// `route_payments_with_swap`.
    ///
    /// Returns the amount of `buy_token` delivered to the recipient (i.e. the
    /// swap output after the platform fee).
    fn process_single_swap_payment(
        env: &Env,
        swap: &SwapPayment,
        platform_treasury: &Address,
        fee_bps: i128,
        fee_cap: i128,
    ) -> Result<i128, Error> {
        swap.sender.require_auth();

        // --- Validate the swap parameters -----------------------------------
        if swap.sell_token == swap.buy_token {
            return Err(Error::InvalidSwapParams);
        }
        if swap.min_amount_out <= 0 {
            return Err(Error::InvalidSwapParams);
        }
        if swap.deadline != 0 && env.ledger().timestamp() > swap.deadline {
            return Err(Error::SwapDeadlineExpired);
        }
        if !Self::is_dex_registered_internal(env, &swap.dex) {
            return Err(Error::DexNotRegistered);
        }

        env.events().publish(
            (Symbol::new(env, "payment_initiated"), swap.sender.clone()),
            swap.amount_in,
        );

        if swap.sender == swap.recipient {
            return Err(Error::InvalidRecipient);
        }
        if Self::is_blacklisted(env.clone(), swap.recipient.clone()) {
            return Err(Error::Blacklisted);
        }

        // --- Amount bounds, applied to the sell side -------------------------
        let max_amount: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MaxAmount)
            .unwrap_or(Self::MAX_AMOUNT);
        if swap.amount_in <= 0 || swap.amount_in > max_amount {
            return Err(Error::LimitExceeded);
        }
        let min_limit: i128 = env
            .storage()
            .instance()
            .get(&DataKey::MinLimit)
            .unwrap_or(0);
        if swap.amount_in < min_limit {
            return Err(Error::LimitExceeded);
        }

        // Daily limits and lifetime volume are denominated in the sell token,
        // matching what the sender actually parts with. A single packed write
        // covers both counters (issue #663), and the returned volume is the
        // pre-payment one the tiered discount keys off.
        let user_volume = Self::accrue_user_record(env, &swap.sender, swap.amount_in)?;
        let effective_fee_bps = if user_volume > Self::VOLUME_THRESHOLD {
            fee_bps / 2
        } else {
            fee_bps
        };

        let sell_token_client = token::Client::new(env, &swap.sell_token);
        if sell_token_client.balance(&swap.sender) < swap.amount_in {
            return Err(Error::InsufficientBalance);
        }

        let contract_address = env.current_contract_address();
        let buy_token_client = token::Client::new(env, &swap.buy_token);
        let buy_balance_before = buy_token_client.balance(&contract_address);

        // --- Pull the sell token in, then swap it ----------------------------
        sell_token_client.transfer(&swap.sender, &contract_address, &swap.amount_in);

        // Hand the input to the adapter: it already holds the sell token and
        // only has to settle the buy token back to this contract. Keeping the
        // pull on this side means the adapter never needs authority over the
        // router's balance.
        sell_token_client.transfer(&contract_address, &swap.dex, &swap.amount_in);

        Self::invoke_dex_swap(
            env,
            &swap.dex,
            &swap.sell_token,
            &swap.buy_token,
            swap.amount_in,
            swap.min_amount_out,
        )?;

        // --- Verify the output against both slippage guards -------------------
        // The balance delta is authoritative: a DEX cannot claim an output it
        // did not actually deliver to this contract.
        let amount_out = buy_token_client.balance(&contract_address) - buy_balance_before;
        if amount_out < swap.min_amount_out {
            log!(env, "Swap output below min_amount_out; reverting payment");
            return Err(Error::SlippageExceeded);
        }
        if swap.expected_amount_out > 0 {
            let max_slippage_bps = Self::max_slippage_bps(env);
            let slippage_floor = Self::slippage_floor(swap.expected_amount_out, max_slippage_bps);
            if amount_out < slippage_floor {
                log!(
                    env,
                    "Swap output exceeded max_slippage_bps; reverting payment"
                );
                return Err(Error::SlippageExceeded);
            }
        }

        env.events().publish(
            (
                Symbol::new(env, "swap_executed"),
                swap.dex.clone(),
                swap.sell_token.clone(),
                swap.buy_token.clone(),
            ),
            (swap.amount_in, amount_out, swap.min_amount_out),
        );

        // --- Fee on the output, then forward the remainder -------------------
        let (fee_amount, remainder) = Self::calculate_fee(amount_out, effective_fee_bps, fee_cap);
        if fee_amount > 0 {
            buy_token_client.transfer(&contract_address, platform_treasury, &fee_amount);
        }
        if remainder > 0 {
            // Mirrors the direct path: if the recipient cannot receive the
            // buy token, the funds stay in the contract and are credited to
            // the sender's refund ledger instead.
            match buy_token_client.try_transfer(&contract_address, &swap.recipient, &remainder) {
                Ok(Ok(())) => {
                    log!(env, "Swapped remainder routed to recipient");
                }
                _ => {
                    log!(
                        env,
                        "Recipient transfer failed; crediting sender refund balance"
                    );
                    Self::credit_refund_balance(env, &swap.sender, &swap.buy_token, remainder);
                }
            }
        }

        env.events().publish(
            (
                symbol_short!("routed"),
                swap.sender.clone(),
                swap.recipient.clone(),
            ),
            remainder,
        );

        log!(env, "Payment routed through DEX swap");

        Ok(remainder)
    }

    /// Routes a payment in any token, swapping it into the recipient's
    /// preferred token on the way.
    ///
    /// The swap-routed counterpart of [`PaymentRouter::route_payment`]: the same
    /// fee, limit, blacklist, and freeze rules apply, with the conversion
    /// inserted between pulling the funds and delivering them. The platform fee
    /// is taken on the `buy_token` output, so `fee_cap` applies in `buy_token`
    /// units for this route.
    ///
    /// # Parameters
    /// - `payment`: The swap-routed transfer (see [`SwapPayment`]).
    ///
    /// # Returns
    /// The amount of `buy_token` delivered to the recipient, after the
    /// platform fee. Otherwise the payment is abandoned whole, with:
    /// - `Err(Error::InvalidSwapParams)`, `Err(Error::SwapDeadlineExpired)`,
    ///   `Err(Error::DexNotRegistered)`, `Err(Error::SwapFailed)`, or
    ///   `Err(Error::SlippageExceeded)` for swap-specific problems,
    /// - the same `Err` variants as `route_payment` otherwise.
    ///
    /// # Panics
    /// Panics if `payment.sender` does not authorize the call, or if a token
    /// transfer out of this contract fails.
    pub fn route_payment_with_swap(env: Env, payment: SwapPayment) -> Result<i128, Error> {
        let _guard = ReentrancyGuard::new(&env)?;
        if Self::is_frozen_internal(&env) {
            return Err(Error::ContractFrozen);
        }
        if Self::is_paused(env.clone()) {
            return Err(Error::Paused);
        }

        let (platform_treasury, fee_bps, fee_cap) = Self::load_fee_config(&env)?;

        Self::process_single_swap_payment(&env, &payment, &platform_treasury, fee_bps, fee_cap)
    }

    /// Routes several swap-routed payments in a single transaction. If any
    /// payment fails, the entire batch is reverted atomically, including any
    /// swaps that already executed earlier in the batch.
    ///
    /// # Parameters
    /// - `payments`: Batch of swap-routed transfers to apply in order. See
    ///   [`SwapPayment`] for per-item constraints.
    ///
    /// # Returns
    /// The total amount of `buy_token` delivered across the batch, or the
    /// first error encountered (see `route_payment_with_swap` for the
    /// possible variants and their causes).
    ///
    /// # Panics
    /// Panics if any payment's `sender` does not authorize the call, or if a
    /// token transfer out of this contract fails.
    pub fn route_payments_with_swap(env: Env, payments: Vec<SwapPayment>) -> Result<i128, Error> {
        let _guard = ReentrancyGuard::new(&env)?;
        if Self::is_frozen_internal(&env) {
            return Err(Error::ContractFrozen);
        }
        // #890: swap routing is blocked under SwapsOnly AND FullHalt.
        Self::require_swaps_unpaused(&env)?;

        let (platform_treasury, fee_bps, fee_cap) = Self::load_fee_config(&env)?;

        let mut total_delivered: i128 = 0;
        for payment in payments.iter() {
            let delivered = Self::process_single_swap_payment(
                &env,
                &payment,
                &platform_treasury,
                fee_bps,
                fee_cap,
            )?;
            total_delivered += delivered;
        }

        Ok(total_delivered)
    }

    /// Asks a registered DEX how much `buy_token` a swap would return, and
    /// derives the `min_amount_out` the sender should use from the contract's
    /// configured slippage ceiling.
    ///
    /// This is a read-only cross-contract call: it moves no funds and changes no
    /// state, so it is safe to call off-chain before building a
    /// [`SwapPayment`].
    ///
    /// # Parameters
    /// - `dex`: Contract ID of a registered DEX adapter.
    /// - `sell_token`: Token the sender would pay with.
    /// - `buy_token`: Token the recipient would be paid in.
    /// - `amount_in`: Amount of `sell_token` to price, in its smallest unit.
    ///
    /// # Returns
    /// A [`SwapQuote`] with the quoted output, the slippage-adjusted
    /// `min_amount_out`, and the slippage ceiling used. Returns
    /// `Err(Error::DexNotRegistered)` if `dex` was never registered or
    /// `Err(Error::SwapFailed)` if the DEX quote call reverts.
    ///
    /// # Panics
    /// Does not panic.
    pub fn quote_swap(
        env: Env,
        dex: Address,
        sell_token: Address,
        buy_token: Address,
        amount_in: i128,
    ) -> Result<SwapQuote, Error> {
        if !Self::is_dex_registered_internal(&env, &dex) {
            return Err(Error::DexNotRegistered);
        }

        let amount_out = Self::invoke_dex_quote(&env, &dex, &sell_token, &buy_token, amount_in)?;
        if amount_out <= 0 {
            return Err(Error::InvalidSwapParams);
        }

        let max_slippage_bps = Self::max_slippage_bps(&env);

        Ok(SwapQuote {
            amount_out,
            min_amount_out: Self::slippage_floor(amount_out, max_slippage_bps),
            max_slippage_bps,
        })
    }

    // ── Swap configuration (sensitive; timelock-gated) ──────────────────────

    /// Allows swap routing to invoke a DEX router contract. Admin-only.
    ///
    /// Restricting cross-contract calls to a registered allowlist is what keeps
    /// swap routing pointed at audited code.
    ///
    /// # Parameters
    /// - `dex`: Contract ID of the DEX router to approve.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract has
    /// no admin set yet.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call.
    ///
    /// DEPRECATED for direct use.  Queue via `queue_action(ActionType::RegisterDex(…))`
    /// and execute after 24 hours.
    pub fn register_dex(env: Env, dex: Address) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        let key = DataKey::RegisteredDex(dex.clone());
        env.storage().persistent().set(&key, &true);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_LIFETIME_THRESHOLD,
            Self::PERSISTENT_BUMP_AMOUNT,
        );

        env.events()
            .publish((Symbol::new(&env, "dex_registered"), admin), dex);

        Ok(())
    }

    /// Stops swap routing from invoking a DEX router contract. Admin-only.
    ///
    /// # Parameters
    /// - `dex`: Contract ID of the DEX router to revoke.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract has
    /// no admin set yet.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call.
    ///
    /// DEPRECATED for direct use.  Queue via `queue_action(ActionType::DeregisterDex(…))`
    /// and execute after 24 hours.
    pub fn deregister_dex(env: Env, dex: Address) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        env.storage()
            .persistent()
            .remove(&DataKey::RegisteredDex(dex.clone()));

        env.events()
            .publish((Symbol::new(&env, "dex_deregistered"), admin), dex);

        Ok(())
    }

    /// Returns whether a DEX router is approved for swap routing.
    ///
    /// # Parameters
    /// - `dex`: Contract ID to check.
    ///
    /// # Returns
    /// `true` if the DEX may be used by `route_payment_with_swap`, `false`
    /// otherwise.
    ///
    /// # Panics
    /// Does not panic.
    pub fn is_dex_registered(env: Env, dex: Address) -> bool {
        Self::is_dex_registered_internal(&env, &dex)
    }

    /// Sets the maximum tolerated swap slippage. Admin-only.
    ///
    /// Applied against the `expected_amount_out` a caller supplies alongside a
    /// quote, as a second guard on top of the per-payment `min_amount_out`
    /// floor.
    ///
    /// # Parameters
    /// - `max_slippage_bps`: New ceiling in basis points; `0` to `10_000`.
    ///
    /// # Returns
    /// `Ok(())` on success, `Err(Error::InvalidSwapParams)` if the value is
    /// outside `0..=10_000`, or `Err(Error::NotInitialized)` if the contract has
    /// no admin set yet.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call.
    ///
    /// DEPRECATED for direct use.  Queue via `queue_action(ActionType::SetMaxSlippageBps(…))`
    /// and execute after 24 hours.
    pub fn set_max_slippage_bps(env: Env, max_slippage_bps: i128) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        if !(0..=Self::MAX_SLIPPAGE_BPS_LIMIT).contains(&max_slippage_bps) {
            return Err(Error::InvalidSwapParams);
        }

        env.storage()
            .instance()
            .set(&DataKey::MaxSlippageBps, &max_slippage_bps);
        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        Ok(())
    }

    /// Returns the maximum tolerated swap slippage in basis points.
    ///
    /// # Returns
    /// The configured `max_slippage_bps`, or the 1 000 bps (10%) default if
    /// the contract has not been initialized.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_max_slippage_bps(env: Env) -> i128 {
        Self::max_slippage_bps(&env)
    }

    /// Returns the available internal refund balance for a user and token.
    ///
    /// # Parameters
    /// - `user`: Address whose refund balance to look up.
    /// - `token`: Contract ID of the token.
    ///
    /// # Returns
    /// The refundable balance for `(user, token)`, or `0` if none is held.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_refund_balance(env: Env, user: Address, token: Address) -> i128 {
        Self::get_refund_balance_internal(&env, &user, &token)
    }

    /// Withdraws a specific amount from the user's internal refund balance.
    ///
    /// A refund balance accrues when a `route_payment` / `route_payments`
    /// transfer to the recipient fails (e.g. missing trustline) and the
    /// funds are held by the contract on the sender's behalf instead.
    ///
    /// # Parameters
    /// - `user`: Address withdrawing funds; must authorize the call.
    /// - `token`: Contract ID of the token to withdraw.
    /// - `amount`: Amount to withdraw. Must be positive and not exceed the
    ///   current refund balance.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NoRefundAvailable)` if `amount`
    /// is zero, negative, or greater than the available balance.
    ///
    /// # Panics
    /// Panics if `user` does not authorize the call, or if the underlying
    /// token transfer fails.
    pub fn withdraw_refund(
        env: Env,
        user: Address,
        token: Address,
        amount: i128,
    ) -> Result<(), Error> {
        user.require_auth();

        if amount <= 0 {
            return Err(Error::NoRefundAvailable);
        }

        let current_balance = Self::get_refund_balance_internal(&env, &user, &token);
        if amount > current_balance {
            return Err(Error::NoRefundAvailable);
        }

        let key = DataKey::RefundBalance(user.clone(), token.clone());
        let new_balance = current_balance - amount;
        if new_balance > 0 {
            env.storage().persistent().set(&key, &new_balance);
            env.storage().persistent().extend_ttl(
                &key,
                Self::PERSISTENT_LIFETIME_THRESHOLD,
                Self::PERSISTENT_BUMP_AMOUNT,
            );
        } else {
            env.storage().persistent().remove(&key);
        }

        let contract_address = env.current_contract_address();
        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&contract_address, &user, &amount);

        env.events().publish(
            (symbol_short!("withdrawn"), user.clone(), token.clone()),
            amount,
        );

        log!(&env, "Refund balance withdrawn by user");
        Ok(())
    }

    /// Claims and withdraws the entire available refund balance for a user and token.
    ///
    /// # Parameters
    /// - `user`: Address withdrawing funds; must authorize the call.
    /// - `token`: Contract ID of the token to withdraw.
    ///
    /// # Returns
    /// `Ok(amount)` with the amount withdrawn, or
    /// `Err(Error::NoRefundAvailable)` if the refund balance is zero.
    ///
    /// # Panics
    /// Panics if `user` does not authorize the call, or if the underlying
    /// token transfer fails.
    pub fn claim_all_refunds(env: Env, user: Address, token: Address) -> Result<i128, Error> {
        user.require_auth();

        let current_balance = Self::get_refund_balance_internal(&env, &user, &token);
        if current_balance <= 0 {
            return Err(Error::NoRefundAvailable);
        }

        Self::withdraw_refund(env, user, token, current_balance)?;
        Ok(current_balance)
    }

    /// Admin-only emergency withdrawal of tokens held by this contract.
    ///
    /// # Parameters
    /// - `token`: Contract ID of the token to withdraw.
    /// Admin-only emergency withdrawal of tokens held by this contract. TreasuryManager-protected.
    ///
    /// # Parameters
    /// - `token`: Contract ID of the token to withdraw.
    /// - `amount`: Amount to transfer from the contract's balance to the treasury manager.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract
    /// has no admin set yet.
    ///
    /// # Panics
    /// Panics if the current TreasuryManager does not authorize the call, or if the
    /// token transfer fails (e.g. the contract's balance is below `amount`).
    pub fn emergency_withdraw(env: Env, token: Address, amount: i128) -> Result<(), Error> {
        let _guard = ReentrancyGuard::new(&env)?;
        let treasury_mgr = Self::require_role(&env, Role::TreasuryManager)?;

        if amount <= 0 {
            return Err(Error::LimitExceeded);
        }

        let token_client = token::Client::new(&env, &token);
        let contract_address = env.current_contract_address();
        if amount > token_client.balance(&contract_address) {
            return Err(Error::InsufficientBalance);
        }
        token_client.transfer(&contract_address, &treasury_mgr, &amount);

        log!(&env, "Emergency withdraw executed by TreasuryManager");
        Ok(())
    }

    // ── Multi-signature (M-of-N) contract upgrades ───────────────────────────
    //
    // Issue #664: upgrades used to be gated on a single admin key, which made
    // that key both a single point of failure (lose it and the contract can
    // never be patched) and a single point of centralization (compromise it and
    // an attacker owns the contract).  Upgrades now require M signatures drawn
    // from an N-member admin group, so no single key — including the admin's —
    // can upgrade the contract on its own.
    //
    // The flow is:
    //   1. The admin configures the group once with `set_multisig_config`.
    //   2. Each signer authorizes a specific WASM hash with `approve_upgrade`.
    //   3. Once M signatures are collected, `upgrade` (or the timelock's
    //      `ActionType::Upgrade`) installs that exact hash and the approvals
    //      are consumed.
    //
    // Until step 1 happens every upgrade fails closed with
    // `Error::MultisigNotInitialized`; there is deliberately no fallback to the
    // single admin key, because that fallback is the vulnerability being fixed.

    // The admin is the root of trust for this call only: it can re-point the
    // group but still cannot upgrade the contract by itself. Prefer
    // `queue_action(ActionType::SetMultisigConfig(…))` to put the 24-hour
    // timelock in front of a rotation, which this direct setter bypasses.
    /// Configures the multi-signature admin group that authorizes upgrades.
    ///
    /// The signer set is replaced wholesale: addresses that are not in
    /// `signers` immediately lose the ability to approve, and a threshold
    /// already reached for a pending hash is re-evaluated against the new
    /// configuration.
    ///
    /// # Parameters
    /// - `signers`: The N addresses whose signatures count. Must be non-empty
    ///   and free of duplicates.
    /// - `threshold`: The M signers required to authorize an upgrade, in
    ///   `1..=signers.len()`.
    ///
    /// # Returns
    /// `Ok(())` on success, `Err(Error::InvalidMultisigConfig)` if the signer
    /// set is empty or holds a duplicate, or the threshold is zero or larger
    /// than the set, or `Err(Error::NotInitialized)` if the contract has no
    /// admin set yet.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call.
    pub fn set_multisig_config(
        env: Env,
        signers: Vec<Address>,
        threshold: u32,
    ) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        let validated = Self::validate_multisig_config(&signers, threshold)?;
        Self::store_multisig_config(&env, signers, validated);

        env.events()
            .publish((Symbol::new(&env, "multisig_config_set"), admin), validated);

        log!(
            &env,
            "Multi-signature upgrade threshold set to {}",
            validated
        );
        Ok(())
    }

    /// Returns the current multi-signature admin group.
    ///
    /// # Returns
    /// The configured signers and threshold, or `Err(Error::MultisigNotInitialized)`
    /// if `set_multisig_config` has not been called yet.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_multisig_config(env: Env) -> Result<MultisigConfig, Error> {
        let (signers, threshold) = Self::load_multisig_config(&env)?;
        Ok(MultisigConfig { signers, threshold })
    }

    /// Records `signer`'s authorization of an upgrade to `new_wasm_hash`.
    ///
    /// # Parameters
    /// - `signer`: The group member approving; must authorize this call.
    /// - `new_wasm_hash`: The WASM hash being approved.
    ///
    /// # Returns
    /// `Ok(())` on success, or appropriate `Error` on failure.
    pub fn approve_upgrade(
        env: Env,
        signer: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<(), Error> {
        let (signers, threshold) = Self::load_multisig_config(&env)?;
        if !signers.contains(&signer) {
            return Err(Error::NotMultisigSigner);
        }
        signer.require_auth();

        let key = DataKey::UpgradeApproval(new_wasm_hash.clone());
        let mut approvals: Vec<Address> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));
        if approvals.contains(&signer) {
            return Err(Error::AlreadyApproved);
        }
        approvals.push_back(signer.clone());
        env.storage().persistent().set(&key, &approvals);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_LIFETIME_THRESHOLD,
            Self::PERSISTENT_BUMP_AMOUNT,
        );

        env.events().publish(
            (Symbol::new(&env, "upgrade_approved"), signer, new_wasm_hash),
            (approvals.len(), threshold),
        );

        log!(
            &env,
            "Upgrade approved by signer {}/{}",
            approvals.len(),
            threshold
        );
        Ok(())
    }

    /// Withdraws a signer's previously recorded approval of an upgrade.
    ///
    /// Lets a signer pull its signature back before the threshold is reached,
    /// which is the way a group stops an upgrade it no longer wants without
    /// having to rotate the whole signer set. Idempotent: withdrawing an
    /// approval that was never recorded is a no-op.
    ///
    /// # Parameters
    /// - `signer`: The group member withdrawing its approval; must authorize
    ///   this call.
    /// - `new_wasm_hash`: The WASM hash to withdraw the approval for.
    ///
    /// # Returns
    /// `Ok(())` on success or `Err(Error::MultisigNotInitialized)` if no group
    /// is configured.
    ///
    /// # Panics
    /// Panics if `signer` does not authorize the call.
    pub fn revoke_upgrade_approval(
        env: Env,
        signer: Address,
        new_wasm_hash: BytesN<32>,
    ) -> Result<(), Error> {
        let (signers, _) = Self::load_multisig_config(&env)?;
        if !signers.contains(&signer) {
            return Err(Error::NotMultisigSigner);
        }
        signer.require_auth();

        let key = DataKey::UpgradeApproval(new_wasm_hash.clone());
        let mut approvals: Vec<Address> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));

        // Vec::first_index_of returns the position of the first match, which
        // for a duplicate-free set is the one and only approval to drop.
        if let Some(index) = approvals.first_index_of(&signer) {
            approvals.remove(index);
            if approvals.is_empty() {
                env.storage().persistent().remove(&key);
            } else {
                env.storage().persistent().set(&key, &approvals);
                env.storage().persistent().extend_ttl(
                    &key,
                    Self::PERSISTENT_LIFETIME_THRESHOLD,
                    Self::PERSISTENT_BUMP_AMOUNT,
                );
            }

            env.events().publish(
                (Symbol::new(&env, "upgrade_revoked"), signer, new_wasm_hash),
                approvals.len(),
            );

            log!(&env, "Upgrade approval revoked");
        }

        Ok(())
    }

    /// Discards every approval collected for `new_wasm_hash`. Admin-only.
    ///
    /// The blunt instrument for a compromised hash: it drops the quorum even
    /// when the threshold was already met, so the group can force the group
    /// back to zero signatures. Note that `upgrade` is permissionless once the
    /// threshold is met, so the admin should prefer having signers revoke their
    /// own approvals (or rotate the group) while the hash is still in flight.
    ///
    /// # Parameters
    /// - `new_wasm_hash`: The WASM hash to clear approvals for.
    ///
    /// # Returns
    /// `Ok(())` on success, or `Err(Error::NotInitialized)` if the contract has
    /// no admin set yet. Clearing a hash with no approvals is a no-op.
    ///
    /// # Panics
    /// Panics if the current admin does not authorize the call.
    pub fn cancel_upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        let admin = Self::require_admin(&env)?;
        admin.require_auth();

        let key = DataKey::UpgradeApproval(new_wasm_hash.clone());
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }

        env.events().publish(
            (Symbol::new(&env, "upgrade_cancelled"), admin),
            new_wasm_hash,
        );

        log!(&env, "Pending upgrade approvals cleared by admin");
        Ok(())
    }

    /// Returns the signers whose approval of an upgrade to `new_wasm_hash`
    /// currently counts.
    ///
    /// Approvals cast by a signer that has since been rotated out of the group
    /// are omitted, so this list always agrees with `is_upgrade_authorized`.
    ///
    /// # Returns
    /// The counted approvals in the order they were recorded, or an empty
    /// vector if the hash has none. Empty when no group is configured.
    ///
    /// # Panics
    /// Does not panic.
    pub fn get_upgrade_approvals(env: Env, new_wasm_hash: BytesN<32>) -> Vec<Address> {
        match Self::load_multisig_config(&env) {
            Ok((signers, _)) => Self::load_effective_approvals(&env, &signers, &new_wasm_hash),
            // Fail closed without erroring: a view of "who approved" is
            // meaningless when there is no group to have authorized anything.
            Err(_) => Vec::new(&env),
        }
    }

    /// Returns whether an upgrade to `new_wasm_hash` is already authorized.
    ///
    /// # Returns
    /// `true` once `M` group members have approved that exact hash.
    /// `Err(Error::MultisigNotInitialized)` if no group is configured.
    ///
    /// # Panics
    /// Does not panic.
    pub fn is_upgrade_authorized(env: Env, new_wasm_hash: BytesN<32>) -> Result<bool, Error> {
        Self::check_upgrade_authorized(&env, &new_wasm_hash)
    }

    // Deliberately permissionless: the M collected signatures *are* the
    // authorization, so whoever submits the transaction once the threshold is
    // met gets the same result, and no additional key — least of all the
    // admin's — can stand in for a quorum. Approvals are consumed on success,
    // so reaching the threshold authorizes exactly one installation. The same
    // gate applies to the timelock path, so queueing an `ActionType::Upgrade`
    // is not a way around it.
    /// Replaces this contract's WASM with a previously uploaded version, once
    /// the multi-signature group has authorized that exact hash.
    ///
    /// # Parameters
    /// - `new_wasm_hash`: Hash of a WASM blob previously uploaded to the
    ///   network. Must match a hash with at least `threshold` approvals.
    ///
    /// # Returns
    /// `Ok(())` on success, `Err(Error::MultisigNotInitialized)` if no group
    /// is configured, or `Err(Error::InsufficientApprovals)` if fewer than
    /// `threshold` members have approved this hash.
    ///
    /// # Panics
    /// Panics if `new_wasm_hash` does not reference an uploaded WASM blob.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        let approvals = Self::require_upgrade_authorized(&env, &new_wasm_hash)?;

        Self::apply_upgrade(&env, &new_wasm_hash);

        env.events().publish(
            (Symbol::new(&env, "contract_upgraded"), new_wasm_hash),
            approvals,
        );

        log!(
            &env,
            "Contract upgraded with {} multisig approvals",
            approvals
        );
        Ok(())
    }

    /// Returns the contract version.
    ///
    /// This is a read-only view function: it does not write to ledger storage
    /// and costs only the base invocation fee.  The UI calls this before
    /// submitting transactions to confirm it is compatible with the deployed
    /// contract.
    ///
    /// # Returns
    /// A [`String`] in the form `"MAJOR.MINOR.PATCH"` (e.g. `"1.0.0"`).
    pub fn version(env: Env) -> String {
        String::from_str(&env, Self::CONTRACT_VERSION)
    }
}

// ── Archival extension ────────────────────────────────────────────────────────
//
// A second #[contractimpl] block keeps the archival surface separate and avoids
// hitting the soroban-sdk per-impl function-count ceiling.
#[contractimpl]
impl PaymentRouter {
    /// Commits a SHA-256 Merkle root of a batch of payment-record snapshots
    /// into persistent storage, opening a new archive epoch.
    ///
    /// Call this before `prune_archived_entries`. Requires TreasuryManager.
    /// Returns the new epoch number.
    ///
    /// Errors: NotInitialized, ContractFrozen.
    pub fn commit_archive_root(
        env: Env,
        root: BytesN<32>,
        leaves: Vec<ArchiveLeaf>,
        description: String,
    ) -> Result<u64, Error> {
        if Self::is_frozen_internal(&env) {
            return Err(Error::ContractFrozen);
        }
        Self::require_role(&env, Role::TreasuryManager)?;

        let current_epoch: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ArchiveEpoch)
            .unwrap_or(0u64);
        let new_epoch = current_epoch + 1;
        env.storage()
            .instance()
            .set(&DataKey::ArchiveEpoch, &new_epoch);

        let record_count = leaves.len();
        let committed_at = env.ledger().timestamp();

        // Persist the Merkle root.
        let root_key = DataKey::ArchiveRoot(new_epoch);
        env.storage().persistent().set(&root_key, &root);
        env.storage().persistent().extend_ttl(
            &root_key,
            Self::PERSISTENT_LIFETIME_THRESHOLD,
            Self::PERSISTENT_BUMP_AMOUNT,
        );

        // Persist metadata.
        let meta = ArchiveMetadata {
            committed_at,
            record_count,
            description,
        };
        let meta_key = DataKey::ArchiveMeta(new_epoch);
        env.storage().persistent().set(&meta_key, &meta);
        env.storage().persistent().extend_ttl(
            &meta_key,
            Self::PERSISTENT_LIFETIME_THRESHOLD,
            Self::PERSISTENT_BUMP_AMOUNT,
        );

        env.storage().instance().extend_ttl(
            Self::INSTANCE_LIFETIME_THRESHOLD,
            Self::INSTANCE_BUMP_AMOUNT,
        );

        archival::emit_archive_committed(&env, new_epoch, &root, record_count);
        log!(
            &env,
            "Archive epoch {} committed: {} records",
            new_epoch,
            record_count
        );

        Ok(new_epoch)
    }

    /// Returns the Merkle root and metadata for an archive epoch, or `None`
    /// if no archive exists for that epoch.
    pub fn get_archive_info(env: Env, epoch: u64) -> Option<(BytesN<32>, ArchiveMetadata)> {
        let root: Option<BytesN<32>> = env.storage().persistent().get(&DataKey::ArchiveRoot(epoch));
        let meta: Option<ArchiveMetadata> =
            env.storage().persistent().get(&DataKey::ArchiveMeta(epoch));
        match (root, meta) {
            (Some(r), Some(m)) => Some((r, m)),
            _ => None,
        }
    }

    /// Returns the current archive epoch counter (0 = no epochs committed yet).
    pub fn get_archive_epoch(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ArchiveEpoch)
            .unwrap_or(0)
    }

    /// Deletes on-chain ledger entries committed via `commit_archive_root`.
    ///
    /// Requires the epoch from a prior commit call. Silently skips absent
    /// entries. Returns the count of entries removed.
    ///
    /// Supported: UserVolume, UserSpending, RefundBalance.
    /// Errors: NotInitialized, ContractFrozen, TimelockNotFound (unknown epoch).
    /// Requires TreasuryManager.
    pub fn prune_archived_entries(
        env: Env,
        committed_epoch: u64,
        leaves: Vec<ArchiveLeaf>,
    ) -> Result<u32, Error> {
        if Self::is_frozen_internal(&env) {
            return Err(Error::ContractFrozen);
        }
        Self::require_role(&env, Role::TreasuryManager)?;

        // Guard: a committed root must exist for this epoch.
        if !env
            .storage()
            .persistent()
            .has(&DataKey::ArchiveRoot(committed_epoch))
        {
            return Err(Error::TimelockNotFound);
        }

        let mut removed: u32 = 0;

        for leaf in leaves.iter() {
            match leaf.record_type {
                ArchiveRecordType::UserVolume => {
                    let key = DataKey::UserVolume(leaf.primary_key.clone());
                    if env.storage().persistent().has(&key) {
                        env.storage().persistent().remove(&key);
                        removed += 1;
                    }
                }
                ArchiveRecordType::UserSpending => {
                    let key = DataKey::UserSpending(leaf.primary_key.clone());
                    if env.storage().persistent().has(&key) {
                        env.storage().persistent().remove(&key);
                        removed += 1;
                    }
                }
                ArchiveRecordType::RefundBalance => {
                    let key = DataKey::RefundBalance(
                        leaf.primary_key.clone(),
                        leaf.secondary_key.clone(),
                    );
                    if env.storage().persistent().has(&key) {
                        env.storage().persistent().remove(&key);
                        removed += 1;
                    }
                }
            }
        }

        env.events().publish(
            (Symbol::new(&env, "entries_pruned"), committed_epoch),
            (removed, env.ledger().timestamp()),
        );

        log!(
            &env,
            "Pruned {} entries for archive epoch {}",
            removed,
            committed_epoch
        );

        Ok(removed)
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod prop_tests;
