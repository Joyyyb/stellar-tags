use soroban_sdk::{contractclient, contracttype, Address, BytesN, Env, Vec};

/// Tiered circuit-breaker levels for the payment router (issue #890).
///
/// Stored as a `u32` in instance storage under [`crate::storage::DataKey::PauseLevel`].
/// The numeric values are stable and form part of the on-chain ABI: never
/// renumber existing variants.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum PauseLevel {
    /// Contract is fully operational; all routing calls are accepted.
    None = 0,
    /// DEX-swap routing is blocked; direct [`route_payment`] /
    /// [`route_payments`] calls continue to function normally.
    SwapsOnly = 1,
    /// Full halt; all routing calls — direct and swap-based — are rejected.
    FullHalt = 2,
}

impl PauseLevel {
    /// Convert a raw `u32` discriminant into a `PauseLevel`, returning
    /// `None` for values outside the defined range.
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(PauseLevel::None),
            1 => Some(PauseLevel::SwapsOnly),
            2 => Some(PauseLevel::FullHalt),
            _ => None,
        }
    }
}

/// A user's rolling 24-hour spending record.
///
/// Retained purely so existing test snapshots that reference this type by
/// name keep compiling. Pre-#663 live contract state was stored as a packed
/// `BytesN<24>`; this struct is not read from or written to storage at runtime.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserSpending {
    /// Unix timestamp (seconds) at which the 24-hour window last reset.
    pub last_reset_time: u64,
    /// Total amount routed by the user since `last_reset_time`.
    pub accumulated_amount: i128,
}

/// A user's combined routing stats, unpacked from the packed `BytesN<40>`
/// `UserRecord` ledger value (issue #663).
///
/// Returned by [`PaymentRouter::get_user_record`] so a client can read both
/// counters in a single view call instead of two.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserRecord {
    /// Total amount routed by the user in the current 24-hour window.
    pub accumulated_amount: i128,
    /// Cumulative lifetime amount routed by the user.
    pub volume: i128,
    /// Unix timestamp (seconds) at which the 24-hour window last reset.
    pub last_reset_time: u64,
}

/// A single transfer instruction for use with [`PaymentRouter::route_payments`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Payment {
    /// Address the funds are debited from. Must authorize the call.
    pub sender: Address,
    /// Address the funds (minus the platform fee) are credited to.
    pub recipient: Address,
    /// Contract ID of the token (or Stellar Asset Contract) being transferred.
    pub token_address: Address,
    /// Amount to route, denominated in the token's smallest unit. Must be
    /// positive and within the contract's configured min/max bounds.
    pub amount: i128,
}

/// Structured payload for meta-transactions.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetaPayment {
    pub nonce: u64,
    pub deadline: u64,
    pub sender: Address,
    pub recipient: Address,
    pub token_address: Address,
    pub amount: i128,
}

/// A single swap-routed transfer instruction for use with
/// [`PaymentRouter::route_payment_with_swap`] and
/// [`PaymentRouter::route_payments_with_swap`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwapPayment {
    /// Address the funds are debited from. Must authorize the call.
    pub sender: Address,
    /// Address the swapped funds (minus the platform fee) are credited to.
    pub recipient: Address,
    /// Token the sender pays with, in that token's smallest unit.
    pub sell_token: Address,
    /// Token the recipient is paid in. Must differ from `sell_token`.
    pub buy_token: Address,
    /// Amount of `sell_token` to pull from the sender and swap.
    pub amount_in: i128,
    /// Minimum amount of `buy_token` the swap must deliver. This is the
    /// slippage floor: if the DEX returns less, the whole payment reverts.
    pub min_amount_out: i128,
    /// Amount of `buy_token` the caller expected from a prior `quote_swap`
    /// call. `0` disables the ceiling check; otherwise the realised output
    /// must stay within the contract's `max_slippage_bps` of this figure.
    pub expected_amount_out: i128,
    /// Unix timestamp (seconds) after which the swap must not execute. `0`
    /// disables the deadline, letting the DEX apply its own.
    pub deadline: u64,
    /// Contract ID of the DEX adapter to invoke. Must be registered by the
    /// admin, which keeps the cross-contract call pointed at audited code.
    pub dex: Address,
}

/// The result of a DEX quote, returned by [`PaymentRouter::quote_swap`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwapQuote {
    /// Amount of `buy_token` the DEX expects to deliver for the quoted input.
    pub amount_out: i128,
    /// Tightest `min_amount_out` that still respects the contract's
    /// `max_slippage_bps` for this quote.
    pub min_amount_out: i128,
    /// Configured maximum slippage, in basis points, that produced
    /// `min_amount_out`.
    pub max_slippage_bps: i128,
}

/// Describes which administrative parameter change a timelock entry represents.
/// Each variant carries all the arguments needed to apply that change when the
/// delay period is over.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionType {
    /// Change the platform treasury address.
    SetPlatformTreasury(Address),
    /// Update fee basis-points and fee cap together (legacy / combined setter).
    SetFeeConfig(i128, i128),
    /// Update fee basis-points only.
    SetFeeBps(i128),
    /// Set the governance contract address.
    SetGovernance(Address),
    /// Change the minimum routing limit.
    SetMinLimit(i128),
    /// Transfer admin rights to a new address.
    TransferAdmin(Address),
    /// Upgrade the contract WASM.
    Upgrade(BytesN<32>),
    /// Arm the dead man's switch: nominate a backup admin and its claim
    /// timeout in seconds.
    SetBackupAdmin(Address, u64),
    /// Allow swap routing to invoke a DEX router contract.
    RegisterDex(Address),
    /// Stop swap routing from invoking a DEX router contract.
    DeregisterDex(Address),
    /// Update the maximum tolerated swap slippage.
    SetMaxSlippageBps(i128),
    /// Update the maximum tolerated slippage for one token pair.
    SetPairMaxSlippageBps(Address, Address, i128),
    /// Configure the multi-signature admin group for contract upgrades.
    SetMultisigConfig(Vec<Address>, u32),
}

/// A pending timelock entry stored in persistent ledger storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelockEntry {
    /// Ledger timestamp (seconds since epoch) when this action was queued.
    pub queued_at: u64,
    /// The action payload to apply once the delay has elapsed.
    pub action: ActionType,
}

/// A single price quote returned by the oracle.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceData {
    /// Fixed-point price value. The true price is `price / 10^decimals`.
    pub price: i128,
    /// Number of decimal places used in `price`.
    pub decimals: u32,
    /// Unix timestamp (seconds) when this price was last updated on-chain.
    pub timestamp: u64,
}

/// A fee change proposal weighted by governance-token balances.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeProposal {
    pub proposer: Address,
    pub fee_bps: i128,
    pub fee_cap: i128,
    pub created_at: u64,
    pub voting_ends_at: u64,
    pub yes_votes: i128,
    pub no_votes: i128,
    pub quorum: i128,
    pub executed: bool,
}

/// Multi-signature configuration for contract upgrades.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultisigConfig {
    /// The list of authorized signers.
    pub signers: Vec<Address>,
    /// The minimum number of signers required to approve an upgrade.
    pub threshold: u32,
}

/// Adapter interface implemented by the DEX used for an arbitrary token swap.
#[contractclient(name = "DexRouterClient")]
pub trait DexRouter {
    fn swap_exact_tokens_for_tokens(
        env: Env,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
        min_amount_out: i128,
        path: Vec<Address>,
        recipient: Address,
    ) -> Vec<i128>;
}

/// Interface implemented by supported Soroban lending protocols.
#[contractclient(name = "LendingProtocolClient")]
pub trait LendingProtocol {
    fn deposit(env: Env, from: Address, token: Address, amount: i128);
    fn withdraw(env: Env, to: Address, token: Address, amount: i128);
    fn harvest(env: Env, to: Address, token: Address) -> i128;
}

/// Minimal interface for an admin-selected KYC issuer or oracle contract.
#[contractclient(name = "KycOracleClient")]
pub trait KycOracle {
    fn is_verified(env: Env, account: Address) -> bool;
}

/// Interface implemented by the admin-selected price-feed oracle.
#[contractclient(name = "PriceFeedOracleClient")]
pub trait PriceFeedOracle {
    fn get_price(env: Env, base_asset: Address, quote_asset: Address) -> PriceData;
}
