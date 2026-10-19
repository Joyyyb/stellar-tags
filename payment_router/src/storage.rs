use crate::errors::Error;
use soroban_sdk::{contracttype, Address, BytesN, Env};

/// Role definitions for the Role-Based Access Control (RBAC) system.
///
/// Segregates operational privileges across dedicated role boundaries:
/// SuperAdmin (root Admin), Pauser, FeeManager, TreasuryManager, and
/// ComplianceOfficer.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Role {
    /// Supreme administrator with exclusive authority over role assignments,
    /// contract upgrades, emergency freeze/unfreeze, and root governance.
    SuperAdmin = 1,
    /// Manager with exclusive authority over platform treasury, yield operations,
    /// token recovery, and emergency asset withdrawals.
    TreasuryManager = 2,
    /// Compliance officer with authority over address blacklisting, KYC oracle
    /// configurations, and oracle price configuration.
    ComplianceOfficer = 3,
    /// Fee manager with authority over platform fee basis points, fee caps, and
    /// minimum payment limits.
    FeeManager = 4,
    /// Pauser with authority over the operational pause switch. Kept separate
    /// from the broader compliance role so the ability to halt routing can be
    /// delegated narrowly.
    Pauser = 5,
}

/// Storage keys for all contract instance and persistent data.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// The current admin address.
    Admin,
    /// Governance contract address; if set, it takes over fee-authority from the admin.
    Governance,
    /// Address that receives collected platform fees.
    PlatformTreasury,
    /// Platform fee rate, in basis points (1/100th of a percent).
    FeeBps,
    /// Upper bound on the fee taken from a single payment.
    FeeCap,
    /// Minimum amount accepted by `route_payment` / `route_payments`, if set.
    MinLimit,
    /// Whether routing is currently paused.
    Paused,
    /// Maximum amount accepted by a single payment.
    MaxAmount,
    /// Cumulative lifetime amount routed by a given sender.
    /// Packed per-user record (issue #663): the 24-hour spending window plus
    /// the cumulative lifetime volume, stored as a single 40-byte value so
    /// registering a sender costs one ledger entry instead of two.
    UserRecord(Address),
    /// DEPRECATED (pre-#663): packed 24-hour spending window for a sender.
    /// No longer written; read only by the `load_user_record` fallback and by
    /// `migrate_user_record`.
    UserSpending(Address),
    /// DEPRECATED (pre-#663): cumulative lifetime amount routed by a sender.
    /// No longer written; read only by the `load_user_record` fallback and by
    /// `migrate_user_record`.
    UserVolume(Address),
    /// Whether a given recipient address is blacklisted.
    Blacklist(Address),
    /// Internal refund balance for a (user, token) pair, credited when a
    /// direct transfer to the recipient fails.
    RefundBalance(Address, Address),
    /// Monotonically-increasing nonce counter used to generate unique IDs for
    /// timelock entries.  Stored as `u64` in instance storage.
    TimelockNonce,
    /// A pending timelock entry keyed by its nonce ID.
    /// Stored in persistent storage so it survives instance eviction.
    TimelockEntry(u64),
    /// When `true` the contract is frozen: payments and timelock executions
    /// are blocked.  Stored as `bool` in instance storage.
    Frozen,
    /// Whether an address holds a given role.
    UserRole(Address, Role),
    /// The primary address currently holding a role.
    Role(Role),
    /// Monotonic nonce for meta-transaction replay protection.
    MetaNonce(Address),
    /// Trusted issuer/oracle queried for high-value payment senders.
    KycOracle,
    /// Payments strictly above this amount require a valid KYC claim.
    KycThreshold,
    /// The backup admin that can claim ownership via the dead man's switch.
    /// Stored as `Address` in instance storage.
    BackupAdmin,
    /// Unix timestamp of the last admin heartbeat (see `ping`).
    /// Stored as `u64` in instance storage.
    LastHeartbeat,
    /// Inactivity window (seconds) after which the backup admin may claim.
    /// Stored as `u64` in instance storage.
    DmsTimeout,
    /// Per-address payment invocation cap per ledger sequence.
    /// Stored as `u32` in instance storage (`0` disables rate limiting).
    RateLimitConfig,
    /// Packed invocation counter for a given sender (BytesN<8>).
    RateLimitCounter(Address),
    /// Whether a given sender is whitelisted and therefore exempt from the rate limiter.
    RateLimitWhitelist(Address),
    /// Address of the price-feed oracle used for fiat/crypto lookups.
    OracleAddress,
    /// Maximum acceptable age, in seconds, of an oracle price quote.
    StalenessThreshold,
    /// Admin-supplied fallback price for a (base, quote) asset pair.
    FallbackPrice(Address, Address),
    /// Token whose balances weight fee-governance votes.
    GovernanceToken,
    /// Minimum weighted vote share required to pass a fee proposal.
    GovernanceQuorum,
    /// Monotonic nonce for fee-proposal ids.
    GovernanceNonce,
    /// A pending fee-change proposal keyed by its id.
    GovernanceProposal(u64),
    /// Recorded yes/no vote weight for a fee proposal.
    GovernanceVote(u64, Address),
    /// Current archival epoch counter, stored in instance storage.
    ArchiveEpoch,
    /// Merkle root committed for an archival epoch.
    ArchiveRoot(u64),
    /// Metadata committed alongside an archival root.
    ArchiveMeta(u64),
    /// Lending protocol contract used for treasury yield operations.
    YieldProtocol,
    /// Principal currently deposited into the yield protocol per token.
    YieldPrincipal(Address),
    /// Whether a DEX router contract is approved to receive cross-contract swap calls.
    RegisteredDex(Address),
    /// Maximum tolerated swap slippage in basis points.
    MaxSlippageBps,
    /// Maximum tolerated swap slippage for a specific input/output token pair.
    PairSlippage(Address, Address),
    /// The N addresses of the multi-signature admin group for upgrades.
    MultisigSigners,
    /// The M signers threshold of the multi-signature admin group.
    MultisigThreshold,
    /// Signer approvals collected for a specific upgrade WASM hash.
    UpgradeApproval(BytesN<32>),
    /// Guard against cross-contract reentrancy attacks.
    ReentrancyGuard,
    /// Tiered pause level for the circuit-breaker (issue #890).
    /// Stored as `u32` in instance storage; see [`crate::types::PauseLevel`].
    /// Replaces the legacy boolean `Paused` key — the old key is still read
    /// during migrations so existing deployments are not bricked.
    PauseLevel,
    /// Address authorised to call `renew_heartbeat` without full admin rights
    /// (issue #889). Stored as `Address` in instance storage.
    HeartbeatOperator,
}

// ── Constants ────────────────────────────────────────────────────────────────

pub const BPS_DIVISOR: i128 = 10_000;
pub const XLM_DECIMALS: i128 = 10_000_000;
pub const MAX_AMOUNT: i128 = 1_000_000_000_000_000; // 100M tokens with 7 decimals
pub const DAILY_MAX_LIMIT: i128 = 1_000_000 * XLM_DECIMALS; // 1M tokens limit
pub const VOLUME_THRESHOLD: i128 = 10_000 * XLM_DECIMALS; // 10,000 XLM threshold for tiered fee discount
pub const SECONDS_IN_24H: u64 = 24 * 3600;
pub const MIN_DMS_TIMEOUT: u64 = 7 * 24 * 3600;
pub const DEFAULT_RATE_LIMIT: u32 = 10;
pub const MAX_RATE_LIMIT: u32 = 1_000_000;
pub const DEFAULT_MAX_SLIPPAGE_BPS: i128 = 1_000;
pub const MAX_SLIPPAGE_BPS_LIMIT: i128 = 10_000;
/// Pair overrides are deliberately stricter than the legacy global ceiling.
pub const MAX_PAIR_SLIPPAGE_BPS: i128 = 500;
pub const DAY_IN_LEDGERS: u32 = 17280;
pub const INSTANCE_BUMP_AMOUNT: u32 = 7 * DAY_IN_LEDGERS;
pub const INSTANCE_LIFETIME_THRESHOLD: u32 = INSTANCE_BUMP_AMOUNT - DAY_IN_LEDGERS;
pub const USER_BUMP_AMOUNT: u32 = 30 * DAY_IN_LEDGERS;
pub const USER_LIFETIME_THRESHOLD: u32 = USER_BUMP_AMOUNT - DAY_IN_LEDGERS;
pub const PERSISTENT_BUMP_AMOUNT: u32 = USER_BUMP_AMOUNT;
pub const PERSISTENT_LIFETIME_THRESHOLD: u32 = USER_LIFETIME_THRESHOLD;

// ── Reentrancy Guard ─────────────────────────────────────────────────────────

pub struct ReentrancyGuard<'a> {
    env: &'a Env,
}

impl<'a> ReentrancyGuard<'a> {
    pub fn new(env: &'a Env) -> Result<Self, Error> {
        let is_locked: bool = env
            .storage()
            .instance()
            .get(&DataKey::ReentrancyGuard)
            .unwrap_or(false);
        if is_locked {
            return Err(Error::ReentrantCall);
        }
        env.storage()
            .instance()
            .set(&DataKey::ReentrancyGuard, &true);
        Ok(Self { env })
    }
}

impl Drop for ReentrancyGuard<'_> {
    fn drop(&mut self) {
        self.env
            .storage()
            .instance()
            .set(&DataKey::ReentrancyGuard, &false);
    }
}

// ── Packed Record Helpers ────────────────────────────────────────────────────

pub fn pack_user_record(
    env: &Env,
    last_reset_time: u64,
    accumulated_amount: i128,
    volume: i128,
) -> BytesN<40> {
    let mut buf = [0u8; 40];
    buf[..8].copy_from_slice(&last_reset_time.to_be_bytes());
    buf[8..24].copy_from_slice(&accumulated_amount.to_be_bytes());
    buf[24..40].copy_from_slice(&volume.to_be_bytes());
    BytesN::from_array(env, &buf)
}

pub fn unpack_user_record(packed: &BytesN<40>) -> (u64, i128, i128) {
    let buf: [u8; 40] = packed.to_array();
    let last_reset_time = u64::from_be_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    let accumulated_amount = i128::from_be_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15], buf[16], buf[17],
        buf[18], buf[19], buf[20], buf[21], buf[22], buf[23],
    ]);
    let volume = i128::from_be_bytes([
        buf[24], buf[25], buf[26], buf[27], buf[28], buf[29], buf[30], buf[31], buf[32], buf[33],
        buf[34], buf[35], buf[36], buf[37], buf[38], buf[39],
    ]);
    (last_reset_time, accumulated_amount, volume)
}

pub fn unpack_legacy_spending(packed: &BytesN<24>) -> (u64, i128) {
    let buf: [u8; 24] = packed.to_array();
    let last_reset_time = u64::from_be_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    let accumulated_amount = i128::from_be_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15], buf[16], buf[17],
        buf[18], buf[19], buf[20], buf[21], buf[22], buf[23],
    ]);
    (last_reset_time, accumulated_amount)
}

pub fn pack_rate_counter(env: &Env, window_ledger: u32, invocations: u32) -> BytesN<8> {
    let mut buf = [0u8; 8];
    let s = window_ledger.to_be_bytes();
    buf[0] = s[0];
    buf[1] = s[1];
    buf[2] = s[2];
    buf[3] = s[3];
    let c = invocations.to_be_bytes();
    buf[4] = c[0];
    buf[5] = c[1];
    buf[6] = c[2];
    buf[7] = c[3];
    BytesN::from_array(env, &buf)
}

pub fn unpack_rate_counter(packed: &BytesN<8>) -> (u32, u32) {
    let buf: [u8; 8] = packed.to_array();
    let window_ledger = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let invocations = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    (window_ledger, invocations)
}

pub fn current_window(env: &Env) -> u32 {
    env.ledger().sequence()
}

pub fn load_user_record(env: &Env, sender: &Address, current_time: u64) -> (u64, i128, i128, bool) {
    let record_key = DataKey::UserRecord(sender.clone());

    if let Some(packed) = env
        .storage()
        .persistent()
        .get::<DataKey, BytesN<40>>(&record_key)
    {
        let (last_reset_time, accumulated_amount, volume) = unpack_user_record(&packed);
        return (last_reset_time, accumulated_amount, volume, false);
    }

    let legacy_spending: Option<(u64, i128)> = env
        .storage()
        .persistent()
        .get(&DataKey::UserSpending(sender.clone()))
        .map(|packed: BytesN<24>| unpack_legacy_spending(&packed));
    let legacy_volume: i128 = env
        .storage()
        .persistent()
        .get(&DataKey::UserVolume(sender.clone()))
        .unwrap_or(0);
    let (last_reset_time, accumulated_amount) = legacy_spending.unwrap_or((current_time, 0));
    let legacy_found = legacy_spending.is_some() || legacy_volume != 0;
    (
        last_reset_time,
        accumulated_amount,
        legacy_volume,
        legacy_found,
    )
}

pub fn accrue_user_record(env: &Env, sender: &Address, amount: i128) -> Result<i128, Error> {
    let current_time = env.ledger().timestamp();
    let record_key = DataKey::UserRecord(sender.clone());

    let (mut last_reset_time, mut accumulated_amount, volume, legacy_found) =
        load_user_record(env, sender, current_time);

    if current_time - last_reset_time >= SECONDS_IN_24H {
        last_reset_time = current_time;
        accumulated_amount = 0;
    }

    let Some(new_accumulated) = accumulated_amount.checked_add(amount) else {
        return Err(Error::LimitExceeded);
    };
    if new_accumulated > DAILY_MAX_LIMIT {
        return Err(Error::LimitExceeded);
    }
    accumulated_amount = new_accumulated;

    let new_volume = volume.saturating_add(amount);

    env.storage().persistent().set(
        &record_key,
        &pack_user_record(env, last_reset_time, accumulated_amount, new_volume),
    );
    env.storage().persistent().extend_ttl(
        &record_key,
        PERSISTENT_LIFETIME_THRESHOLD,
        PERSISTENT_BUMP_AMOUNT,
    );

    if legacy_found {
        env.storage()
            .persistent()
            .remove(&DataKey::UserSpending(sender.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::UserVolume(sender.clone()));
    }

    Ok(volume)
}
