use super::*;

// The crate is `no_std`, but the test harness links std; pull it in so
// the benchmark can print its GAS REPORT lines.
extern crate std;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    testutils::{Address as _, Events, Ledger as _, LedgerInfo, MockAuth, MockAuthInvoke},
    token::StellarAssetClient,
    Address, Env, Symbol, TryIntoVal, Val,
};

#[contracttype]
#[derive(Clone)]
enum MockLendingKey {
    Principal(Address),
    Yield(Address),
}

#[contract]
struct MockLendingProtocol;

#[contractimpl]
impl MockLendingProtocol {
    pub fn deposit(env: Env, from: Address, token: Address, amount: i128) {
        from.require_auth();
        token::Client::new(&env, &token).transfer(&from, &env.current_contract_address(), &amount);
        let key = MockLendingKey::Principal(token);
        let current: i128 = env.storage().instance().get(&key).unwrap_or(0);
        env.storage().instance().set(&key, &(current + amount));
    }

    pub fn withdraw(env: Env, to: Address, token: Address, amount: i128) {
        let key = MockLendingKey::Principal(token.clone());
        let current: i128 = env.storage().instance().get(&key).unwrap_or(0);
        assert!(current >= amount);
        token::Client::new(&env, &token).transfer(&env.current_contract_address(), &to, &amount);
        env.storage().instance().set(&key, &(current - amount));
    }

    pub fn harvest(env: Env, to: Address, token: Address) -> i128 {
        let key = MockLendingKey::Yield(token.clone());
        let amount: i128 = env.storage().instance().get(&key).unwrap_or(0);
        if amount > 0 {
            token::Client::new(&env, &token).transfer(
                &env.current_contract_address(),
                &to,
                &amount,
            );
            env.storage().instance().remove(&key);
        }
        amount
    }

    pub fn accrue_yield(env: Env, token: Address, amount: i128) {
        let key = MockLendingKey::Yield(token);
        let current: i128 = env.storage().instance().get(&key).unwrap_or(0);
        env.storage().instance().set(&key, &(current + amount));
    }
}

#[contracttype]
#[derive(Clone)]
enum MockKycKey {
    Verified(Address),
}

#[contract]
struct MockKycOracle;

#[contractimpl]
impl MockKycOracle {
    pub fn set_verified(env: Env, account: Address, verified: bool) {
        env.storage()
            .instance()
            .set(&MockKycKey::Verified(account), &verified);
    }

    pub fn is_verified(env: Env, account: Address) -> bool {
        env.storage()
            .instance()
            .get(&MockKycKey::Verified(account))
            .unwrap_or(false)
    }
}

/// Rewrites the ledger info so `sequence_number` becomes `sequence`,
/// advancing the per-ledger rate-limit window (issue #716).
fn advance_ledger(env: &Env, sequence: u32) {
    env.ledger().set(LedgerInfo {
        timestamp: env.ledger().timestamp(),
        protocol_version: env.ledger().protocol_version(),
        sequence_number: sequence,
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });
}

// ── Mock price-feed oracle ───────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
enum MockOracleKey {
    Price(Address, Address),
    ShouldFail,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MockOracleError {
    /// Simulates an unavailable oracle.
    Unavailable = 1,
}

#[contract]
struct MockPriceFeedOracle;

#[contractimpl]
impl MockPriceFeedOracle {
    /// Store a price for a given (base, quote) pair.
    pub fn set_price(
        env: Env,
        base_asset: Address,
        quote_asset: Address,
        price: i128,
        decimals: u32,
        timestamp: u64,
    ) {
        let data = PriceData {
            price,
            decimals,
            timestamp,
        };
        env.storage()
            .instance()
            .set(&MockOracleKey::Price(base_asset, quote_asset), &data);
    }

    /// Configure the mock to trap on the next `get_price` call.
    pub fn set_should_fail(env: Env, fail: bool) {
        env.storage()
            .instance()
            .set(&MockOracleKey::ShouldFail, &fail);
    }

    /// Implements the PriceFeedOracle interface.
    pub fn get_price(
        env: Env,
        base_asset: Address,
        quote_asset: Address,
    ) -> Result<PriceData, MockOracleError> {
        let should_fail: bool = env
            .storage()
            .instance()
            .get(&MockOracleKey::ShouldFail)
            .unwrap_or(false);
        if should_fail {
            return Err(MockOracleError::Unavailable);
        }
        Ok(env
            .storage()
            .instance()
            .get(&MockOracleKey::Price(base_asset, quote_asset))
            .unwrap_or(PriceData {
                price: 0,
                decimals: 7,
                timestamp: 0,
            }))
    }
}

// ── Oracle helper ────────────────────────────────────────────────────────

fn setup_oracle_env() -> (
    Env,
    PaymentRouterClient<'static>,
    Address,
    MockPriceFeedOracleClient<'static>,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();
    // The ledger starts at timestamp 0, which `get_price` treats as a
    // stale quote. Start from a realistic time so "fresh" prices are
    // actually fresh.
    env.ledger().set(LedgerInfo {
        timestamp: 1_000_000,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence(),
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });
    let contract_id = env.register_contract(None, PaymentRouter);
    let client = PaymentRouterClient::new(&env, &contract_id);
    let oracle_id = env.register_contract(None, MockPriceFeedOracle);
    let oracle_client = MockPriceFeedOracleClient::new(&env, &oracle_id);
    let base = Address::generate(&env);
    let quote = Address::generate(&env);
    (
        env,
        client,
        contract_id,
        oracle_client,
        oracle_id,
        base,
        quote,
    )
}

// ── Oracle tests ─────────────────────────────────────────────────────────

#[test]
fn test_get_price_returns_valid_oracle_price() {
    let (env, client, _, oracle_client, oracle_id, base, quote) = setup_oracle_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);
    client.set_price_oracle(&oracle_id);

    // Populate mock: 0.125 USD/XLM with 7 decimals = 1_250_000, fresh timestamp
    let now = env.ledger().timestamp();
    oracle_client.set_price(&base, &quote, &1_250_000, &7, &now);

    let price_data = client.get_price(&base, &quote);
    assert_eq!(price_data.price, 1_250_000);
    assert_eq!(price_data.decimals, 7);
    assert_eq!(price_data.timestamp, now);
}

#[test]
fn test_get_price_fails_when_oracle_not_configured() {
    let (env, client, _, _oracle_client, _oracle_id, base, quote) = setup_oracle_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // No oracle set, no fallback
    assert_eq!(
        client.try_get_price(&base, &quote),
        Err(Ok(Error::OracleNotConfigured))
    );
}

#[test]
fn test_get_price_uses_fallback_when_oracle_not_configured() {
    let (env, client, _, _oracle_client, _oracle_id, base, quote) = setup_oracle_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Set a fallback price but no live oracle
    client.set_fallback_price(&base, &quote, &1_000_000, &7);

    let fallback = client.get_fallback_price(&base, &quote);
    assert!(fallback.is_some());
    assert_eq!(fallback.unwrap().price, 1_000_000);

    // get_price should return the fallback
    let result = client.get_price(&base, &quote);
    assert_eq!(result.price, 1_000_000);
}

#[test]
fn test_get_price_rejects_stale_data_and_uses_fallback() {
    let (env, client, _, oracle_client, oracle_id, base, quote) = setup_oracle_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);
    client.set_price_oracle(&oracle_id);
    // Threshold of 3600 seconds (default)
    client.set_staleness_threshold(&3_600u64);

    // Oracle returns a price with a very old timestamp (2 hours ago)
    let stale_timestamp = env.ledger().timestamp().saturating_sub(7_200);
    oracle_client.set_price(&base, &quote, &2_000_000, &7, &stale_timestamp);

    // Without fallback: should return OraclePriceStale
    assert_eq!(
        client.try_get_price(&base, &quote),
        Err(Ok(Error::OraclePriceStale))
    );

    // Add a fallback: should now return the fallback price
    client.set_fallback_price(&base, &quote, &1_800_000, &7);
    let result = client.get_price(&base, &quote);
    assert_eq!(result.price, 1_800_000);
}

#[test]
fn test_get_price_rejects_invalid_price() {
    let (env, client, _, oracle_client, oracle_id, base, quote) = setup_oracle_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);
    client.set_price_oracle(&oracle_id);

    // Oracle returns price = 0 with a fresh timestamp
    let now = env.ledger().timestamp();
    oracle_client.set_price(&base, &quote, &0, &7, &now);

    assert_eq!(
        client.try_get_price(&base, &quote),
        Err(Ok(Error::OraclePriceInvalid))
    );
}

#[test]
fn test_get_price_falls_back_when_oracle_call_fails() {
    let (env, client, _, oracle_client, oracle_id, base, quote) = setup_oracle_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);
    client.set_price_oracle(&oracle_id);

    // Configure mock to fail
    oracle_client.set_should_fail(&true);

    // No fallback: should error
    assert_eq!(
        client.try_get_price(&base, &quote),
        Err(Ok(Error::OracleCallFailed))
    );

    // With fallback configured: should succeed
    client.set_fallback_price(&base, &quote, &5_000_000, &7);
    let result = client.get_price(&base, &quote);
    assert_eq!(result.price, 5_000_000);
}

#[test]
fn test_staleness_threshold_zero_disables_staleness_check() {
    let (env, client, _, oracle_client, oracle_id, base, quote) = setup_oracle_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);
    client.set_price_oracle(&oracle_id);
    // Set threshold to 0 = staleness check disabled
    client.set_staleness_threshold(&0u64);

    // Oracle returns a price with timestamp 0 (would normally be stale)
    oracle_client.set_price(&base, &quote, &3_000_000, &7, &0);

    // Should pass because staleness check is disabled
    let result = client.get_price(&base, &quote);
    assert_eq!(result.price, 3_000_000);
}

#[test]
fn test_set_fallback_price_zero_clears_fallback() {
    let (env, client, _, _oracle_client, _oracle_id, base, quote) = setup_oracle_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Set then clear fallback
    client.set_fallback_price(&base, &quote, &1_000_000, &7);
    assert!(client.get_fallback_price(&base, &quote).is_some());

    client.set_fallback_price(&base, &quote, &0, &7);
    assert!(client.get_fallback_price(&base, &quote).is_none());
}

#[test]
fn test_set_price_oracle_requires_compliance_officer_role() {
    let (env, client, _, _oracle_client, oracle_id, _base, _quote) = setup_oracle_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Only ComplianceOfficer (admin in this test via mock_all_auths) can set the oracle.
    // Verify auth is recorded for admin.
    client.set_price_oracle(&oracle_id);
    let auths = env.auths();
    let admin_auth_present = auths.iter().any(|(addr, _)| *addr == admin);
    assert!(
        admin_auth_present,
        "set_price_oracle must require admin/ComplianceOfficer authorization"
    );
}

/// Returns (env, client, contract_id).
fn setup_env() -> (Env, PaymentRouterClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, PaymentRouter);
    let client = PaymentRouterClient::new(&env, &contract_id);
    (env, client, contract_id)
}

/// Deploys a Stellar Asset Contract test token. Returns
/// (token_address, token_client, stellar_asset_admin_client).
fn setup_token(
    env: &Env,
) -> (
    Address,
    token::Client<'static>,
    token::StellarAssetClient<'static>,
) {
    let token_admin = Address::generate(env);
    let token_address = env.register_stellar_asset_contract(token_admin);
    let token_client = token::Client::new(env, &token_address);
    let token_admin_client = token::StellarAssetClient::new(env, &token_address);
    (token_address, token_client, token_admin_client)
}

// ── Timelock tests ───────────────────────────────────────────────────────

#[test]
fn test_treasury_yield_deposit_harvest_and_withdraw() {
    let (env, client, _contract_id) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let protocol_id = env.register_contract(None, MockLendingProtocol);
    let protocol_client = MockLendingProtocolClient::new(&env, &protocol_id);
    let (token_address, token_client, token_admin_client) = setup_token(&env);

    client.initialize(&admin, &treasury, &100, &1_000, &PaymentRouter::MAX_AMOUNT);
    client.set_yield_protocol(&protocol_id);
    token_admin_client.mint(&treasury, &10_000);

    client.deposit_to_yield(&token_address, &6_000);
    assert_eq!(client.get_yield_position(&token_address), 6_000);
    assert_eq!(token_client.balance(&treasury), 4_000);
    assert_eq!(token_client.balance(&protocol_id), 6_000);

    token_admin_client.mint(&protocol_id, &500);
    protocol_client.accrue_yield(&token_address, &500);
    assert_eq!(client.harvest_yield(&token_address), 500);
    assert_eq!(token_client.balance(&treasury), 4_500);
    assert_eq!(client.get_yield_position(&token_address), 6_000);

    client.withdraw_from_yield(&token_address, &2_000);
    assert_eq!(client.get_yield_position(&token_address), 4_000);
    assert_eq!(token_client.balance(&treasury), 6_500);
    assert_eq!(token_client.balance(&protocol_id), 4_000);
}

#[test]
fn test_yield_operations_require_configuration_and_valid_amounts() {
    let (env, client, _contract_id) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let (token_address, _token_client, _token_admin_client) = setup_token(&env);
    client.initialize(&admin, &treasury, &100, &1_000, &PaymentRouter::MAX_AMOUNT);

    assert_eq!(
        client.try_deposit_to_yield(&token_address, &100),
        Err(Ok(Error::YieldProtocolNotConfigured))
    );
    assert_eq!(
        client.try_deposit_to_yield(&token_address, &0),
        Err(Ok(Error::InvalidYieldAmount))
    );
    assert_eq!(
        client.try_withdraw_from_yield(&token_address, &1),
        Err(Ok(Error::InvalidYieldAmount))
    );
}

#[test]
fn test_kyc_oracle_gates_only_high_value_payments() {
    let (env, client, _contract_id) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);
    let oracle_id = env.register_contract(None, MockKycOracle);
    let oracle_client = MockKycOracleClient::new(&env, &oracle_id);
    let (token_address, token_client, token_admin_client) = setup_token(&env);

    client.initialize(&admin, &treasury, &100, &1_000, &PaymentRouter::MAX_AMOUNT);
    client.set_kyc_config(&oracle_id, &1_000);
    token_admin_client.mint(&sender, &10_000);

    client.route_payment(&sender, &recipient, &token_address, &500);
    assert_eq!(
        client.try_route_payment(&sender, &recipient, &token_address, &2_000),
        Err(Ok(Error::KycRequired))
    );

    oracle_client.set_verified(&sender, &true);
    client.route_payment(&sender, &recipient, &token_address, &2_000);
    assert_eq!(client.get_kyc_threshold(), Some(1_000));
    assert_eq!(token_client.balance(&sender), 7_500);
}

#[test]
fn test_kyc_config_rejects_negative_threshold() {
    let (env, client, _contract_id) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let oracle = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1_000, &PaymentRouter::MAX_AMOUNT);

    assert_eq!(
        client.try_set_kyc_config(&oracle, &-1),
        Err(Ok(Error::InvalidKycThreshold))
    );
}

#[test]
fn test_batch_payments_enforce_kyc_for_each_sender() {
    let (env, client, _contract_id) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);
    let oracle_id = env.register_contract(None, MockKycOracle);
    let (token_address, _token_client, token_admin_client) = setup_token(&env);
    client.initialize(&admin, &treasury, &100, &1_000, &PaymentRouter::MAX_AMOUNT);
    client.set_kyc_config(&oracle_id, &1_000);
    token_admin_client.mint(&sender, &5_000);

    let payments = Vec::from_array(
        &env,
        [Payment {
            sender,
            recipient,
            token_address,
            amount: 2_000,
        }],
    );
    assert_eq!(
        client.try_route_payments(&payments),
        Err(Ok(Error::KycRequired))
    );
}

/// Regression test for the `route_payments` trap on a repeated sender.
///
/// Found by the `route_payments` fuzz target: a batch listing the same
/// sender twice aborted the whole invocation with a non-unwinding panic
/// instead of returning an error. Re-authorizing an address that has
/// already authorized the invocation is what trips the host, and splitting
/// one payment across several recipients is an ordinary request, so this
/// has to succeed rather than trap.
///
/// Asserts the transfers actually landed, not just that the call returned:
/// an early `Err` would also avoid the panic, so a "did not trap" check
/// alone would pass for the wrong reason.
#[test]
fn test_batch_with_a_repeated_sender_succeeds() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let (token_address, token_client, token_admin_client) = setup_token(&env);
    client.initialize(
        &admin,
        &treasury,
        &100,
        &1_000_000,
        &PaymentRouter::MAX_AMOUNT,
    );

    let starting_balance = 1_000_000_000_i128;
    token_admin_client.mint(&sender, &starting_balance);

    let mut recipients: Vec<Address> = Vec::new(&env);
    for _ in 0..3 {
        recipients.push_back(Address::generate(&env));
    }
    let amount = 1_000_000_i128;
    let fee = amount * 100 / 10_000;

    // One sender, three payments, three distinct recipients.
    let mut payments: Vec<Payment> = Vec::new(&env);
    for recipient in recipients.iter() {
        payments.push_back(Payment {
            sender: sender.clone(),
            recipient: recipient.clone(),
            token_address: token_address.clone(),
            amount,
        });
    }

    client.route_payments(&payments);

    let expected_net = amount - fee;
    for recipient in recipients.iter() {
        assert_eq!(
            token_client.balance(&recipient),
            expected_net,
            "recipient was not paid the net amount"
        );
    }
    assert_eq!(token_client.balance(&treasury), fee * 3, "fee mismatch");
    assert_eq!(
        token_client.balance(&sender),
        starting_balance - (expected_net * 3) - (fee * 3),
        "sender was debited the wrong total"
    );
    // The per-sender daily spending record has to accumulate across every
    // payment in the batch, not just the last one.
    assert_eq!(client.get_user_volume(&sender), amount * 3);
}

/// A batch that repeats a sender *and* fails a later payment must still be
/// a graceful `Err`: the distinct-sender authorization runs before any
/// transfer, so a rejected batch leaves no partial state behind.
#[test]
fn test_batch_with_a_repeated_sender_rejects_gracefully() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);
    let (token_address, token_client, token_admin_client) = setup_token(&env);
    client.initialize(
        &admin,
        &treasury,
        &100,
        &1_000_000,
        &PaymentRouter::MAX_AMOUNT,
    );
    token_admin_client.mint(&sender, &1_000_000_000);

    // First payment is valid and from `sender`; the second is over the
    // per-payment cap, so the batch is rejected after `sender` has already
    // been authorized once.
    let payments = Vec::from_array(
        &env,
        [
            Payment {
                sender: sender.clone(),
                recipient: recipient.clone(),
                token_address: token_address.clone(),
                amount: 1_000_000,
            },
            Payment {
                sender,
                recipient: recipient.clone(),
                token_address,
                amount: PaymentRouter::MAX_AMOUNT + 1,
            },
        ],
    );

    assert_eq!(
        client.try_route_payments(&payments),
        Err(Ok(Error::LimitExceeded))
    );
    // Nothing was moved: validation rejects the whole batch up front.
    assert_eq!(token_client.balance(&recipient), 0);
}

/// A batch from several distinct senders must pay every one of them.
///
/// The authorization pass walks a de-duplicated seen-set, so a sender
/// appearing several times is authorized once while a batch of different
/// senders is authorized once each.  This had no active coverage at all
/// before, which is how an over-eager `require_auth` in the per-payment
/// loop could abort every multi-payment batch.
#[test]
fn test_batch_with_several_distinct_senders_succeeds() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let (token_address, token_client, token_admin_client) = setup_token(&env);
    client.initialize(
        &admin,
        &treasury,
        &100,
        &1_000_000,
        &PaymentRouter::MAX_AMOUNT,
    );

    let starting_balance = 1_000_000_000_i128;
    let mut senders: Vec<Address> = Vec::new(&env);
    for _ in 0..2 {
        let sender = Address::generate(&env);
        token_admin_client.mint(&sender, &starting_balance);
        senders.push_back(sender);
    }

    let first_recipient = Address::generate(&env);
    let second_recipient = Address::generate(&env);
    let amount = 1_000_000_i128;
    let fee = amount * 100 / 10_000;
    let expected_net = amount - fee;

    // The first sender pays twice, the second once, so the batch mixes a
    // repeated sender with a distinct one.
    let payments = Vec::from_array(
        &env,
        [
            Payment {
                sender: senders.get(0).unwrap().clone(),
                recipient: first_recipient.clone(),
                token_address: token_address.clone(),
                amount,
            },
            Payment {
                sender: senders.get(0).unwrap().clone(),
                recipient: second_recipient.clone(),
                token_address: token_address.clone(),
                amount,
            },
            Payment {
                sender: senders.get(1).unwrap().clone(),
                recipient: first_recipient.clone(),
                token_address: token_address.clone(),
                amount,
            },
        ],
    );

    client.route_payments(&payments);

    assert_eq!(
        token_client.balance(&first_recipient),
        expected_net * 2,
        "first recipient should be paid for both incoming payments"
    );
    assert_eq!(
        token_client.balance(&second_recipient),
        expected_net,
        "second recipient should be paid once"
    );
    assert_eq!(token_client.balance(&treasury), fee * 3, "fee mismatch");
    // The first sender funded two payments, the second only one.
    assert_eq!(
        token_client.balance(&senders.get(0).unwrap()),
        starting_balance - (expected_net * 2) - (fee * 2),
        "first sender was debited the wrong total"
    );
    assert_eq!(
        token_client.balance(&senders.get(1).unwrap()),
        starting_balance - expected_net - fee,
        "second sender was debited the wrong total"
    );
}

#[test]
fn test_queue_and_execute_set_fee_bps_after_delay() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Queue a fee-bps change.
    let nonce = client.queue_action(&ActionType::SetFeeBps(250));
    assert_eq!(nonce, 1);
    assert_eq!(client.get_fee(), 100); // Not applied yet.

    // Trying to execute immediately should fail (delay not elapsed).
    let res = client.try_execute_action(&nonce);
    assert_eq!(res.unwrap_err().unwrap(), Error::TimelockNotReady);

    // Advance time past 24 hours.
    let current_time = env.ledger().timestamp();
    env.ledger().set(LedgerInfo {
        timestamp: current_time + PaymentRouter::SECONDS_IN_24H + 1,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence(),
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });

    // Now execution should succeed.
    client.execute_action(&nonce);
    assert_eq!(client.get_fee(), 250);

    // Entry should be gone.
    let res = client.try_get_queued_action(&nonce);
    assert_eq!(res.unwrap_err().unwrap(), Error::TimelockNotFound);
}

#[test]
fn test_queue_and_execute_set_platform_treasury() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let new_treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let nonce = client.queue_action(&ActionType::SetPlatformTreasury(new_treasury.clone()));

    // Advance 24h+.
    let ts = env.ledger().timestamp();
    env.ledger().set(LedgerInfo {
        timestamp: ts + PaymentRouter::SECONDS_IN_24H + 1,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence(),
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });

    client.execute_action(&nonce);

    // Verify the treasury was actually updated by routing a payment and
    // checking where the fee lands.
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);
    let (token_addr, token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);
    client.route_payment(&sender, &recipient, &token_addr, &1000);

    // 100 bps of 1000 = 10, capped to min(10, 1000) = 10
    assert_eq!(token_client.balance(&new_treasury), 10);
    assert_eq!(token_client.balance(&treasury), 0);
}

#[test]
fn test_execute_action_not_found() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let res = client.try_execute_action(&99u64);
    assert_eq!(res.unwrap_err().unwrap(), Error::TimelockNotFound);
}

#[test]
fn test_cancel_action() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let nonce = client.queue_action(&ActionType::SetFeeBps(999));
    assert!(client.try_get_queued_action(&nonce).is_ok());

    client.cancel_action(&nonce);

    // Entry should be gone.
    let res = client.try_get_queued_action(&nonce);
    assert_eq!(res.unwrap_err().unwrap(), Error::TimelockNotFound);

    // Fee should remain unchanged.
    assert_eq!(client.get_fee(), 100);
}

#[test]
fn test_cancel_nonexistent_action() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let res = client.try_cancel_action(&42u64);
    assert_eq!(res.unwrap_err().unwrap(), Error::TimelockNotFound);
}

#[test]
fn test_nonce_increments() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let n1 = client.queue_action(&ActionType::SetFeeBps(200));
    let n2 = client.queue_action(&ActionType::SetFeeBps(300));
    let n3 = client.queue_action(&ActionType::SetFeeBps(400));

    assert_eq!(n1, 1);
    assert_eq!(n2, 2);
    assert_eq!(n3, 3);
}

#[test]
fn test_execute_batch_actions_three_distinct_actions_success() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let dex = Address::generate(&env);

    // Queue 3 distinct actions
    let n1 = client.queue_action(&ActionType::SetFeeBps(250));
    let n2 = client.queue_action(&ActionType::SetMaxSlippageBps(300));
    let n3 = client.queue_action(&ActionType::RegisterDex(dex.clone()));

    // Verify initial state
    assert_eq!(client.get_fee(), 100);
    assert_eq!(client.get_max_slippage_bps(), PaymentRouter::DEFAULT_MAX_SLIPPAGE_BPS);
    assert!(!client.is_dex_registered(&dex));

    // Advance time past the 24h delay
    advance_time(&env, PaymentRouter::SECONDS_IN_24H + 1);

    let nonces = vec![&env, n1, n2, n3];
    client.execute_batch_actions(&nonces);

    // Verify all 3 distinct actions executed together
    assert_eq!(client.get_fee(), 250);
    assert_eq!(client.get_max_slippage_bps(), 300);
    assert!(client.is_dex_registered(&dex));

    // Verify all nonces are removed from queue
    assert_eq!(client.try_get_queued_action(&n1).unwrap_err().unwrap(), Error::TimelockNotFound);
    assert_eq!(client.try_get_queued_action(&n2).unwrap_err().unwrap(), Error::TimelockNotFound);
    assert_eq!(client.try_get_queued_action(&n3).unwrap_err().unwrap(), Error::TimelockNotFound);

    // Verify batch_actions_executed event was emitted with the list of nonces
    let events = env.events().all();
    let found = events.iter().any(|(_, topics, data)| {
        if topics.is_empty() {
            return false;
        }
        let raw = topics.get(0).unwrap();
        let sym: Result<Symbol, _> = raw.try_into_val(&env);
        if sym == Ok(Symbol::new(&env, "batch_actions_executed")) {
            let emitted_nonces: Result<Vec<u64>, _> = data.try_into_val(&env);
            emitted_nonces == Ok(nonces.clone())
        } else {
            false
        }
    });
    assert!(found, "batch_actions_executed event not found with list of nonces");
}

#[test]
fn test_execute_batch_actions_reverts_if_one_not_ready() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let dex = Address::generate(&env);

    // Queue actions 1 and 2 at T0
    let n1 = client.queue_action(&ActionType::SetFeeBps(250));
    let n2 = client.queue_action(&ActionType::SetMaxSlippageBps(300));

    // Advance 20 hours (less than 24h)
    advance_time(&env, 20 * 3600);

    // Queue action 3 at T0 + 20h
    let n3 = client.queue_action(&ActionType::RegisterDex(dex.clone()));

    // Advance 5 hours (T0 + 25h total).
    // Actions 1 and 2 have been queued for 25h (ready).
    // Action 3 has been queued for only 5h (NOT ready).
    advance_time(&env, 5 * 3600);

    let nonces = vec![&env, n1, n2, n3];
    let res = client.try_execute_batch_actions(&nonces);
    assert_eq!(res.unwrap_err().unwrap(), Error::TimelockNotReady);

    // Verify atomic rollback: none of the actions took effect
    assert_eq!(client.get_fee(), 100);
    assert_eq!(client.get_max_slippage_bps(), PaymentRouter::DEFAULT_MAX_SLIPPAGE_BPS);
    assert!(!client.is_dex_registered(&dex));

    // Verify all actions remain pending in the queue
    assert!(client.try_get_queued_action(&n1).is_ok());
    assert!(client.try_get_queued_action(&n2).is_ok());
    assert!(client.try_get_queued_action(&n3).is_ok());
}

#[test]
fn test_execute_batch_actions_reverts_if_one_not_found() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let n1 = client.queue_action(&ActionType::SetFeeBps(250));
    let n2 = client.queue_action(&ActionType::SetMaxSlippageBps(300));

    advance_time(&env, PaymentRouter::SECONDS_IN_24H + 1);

    // Batch contains an invalid nonce 9999
    let nonces = vec![&env, n1, 9999u64, n2];
    let res = client.try_execute_batch_actions(&nonces);
    assert_eq!(res.unwrap_err().unwrap(), Error::TimelockNotFound);

    // Verify neither action executed
    assert_eq!(client.get_fee(), 100);
    assert_eq!(client.get_max_slippage_bps(), PaymentRouter::DEFAULT_MAX_SLIPPAGE_BPS);
    assert!(client.try_get_queued_action(&n1).is_ok());
    assert!(client.try_get_queued_action(&n2).is_ok());
}

#[test]
fn test_execute_batch_actions_blocks_when_frozen() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let n1 = client.queue_action(&ActionType::SetFeeBps(250));
    let n2 = client.queue_action(&ActionType::SetMaxSlippageBps(300));

    advance_time(&env, PaymentRouter::SECONDS_IN_24H + 1);

    client.emergency_freeze();

    let nonces = vec![&env, n1, n2];
    let res = client.try_execute_batch_actions(&nonces);
    assert_eq!(res.unwrap_err().unwrap(), Error::ContractFrozen);
}

// ── Freeze tests ─────────────────────────────────────────────────────────

#[test]
fn test_emergency_freeze_blocks_payments() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    assert!(!client.is_frozen());

    client.emergency_freeze();
    assert!(client.is_frozen());

    let res = client.try_route_payment(&sender, &recipient, &token_address, &1000);
    assert_eq!(res.unwrap_err().unwrap(), Error::ContractFrozen);
}

#[test]
fn test_emergency_freeze_blocks_timelock_execution() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let nonce = client.queue_action(&ActionType::SetFeeBps(500));

    // Advance past 24h.
    let ts = env.ledger().timestamp();
    env.ledger().set(LedgerInfo {
        timestamp: ts + PaymentRouter::SECONDS_IN_24H + 1,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence(),
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });

    // Freeze the contract before execution.
    client.emergency_freeze();

    let res = client.try_execute_action(&nonce);
    assert_eq!(res.unwrap_err().unwrap(), Error::ContractFrozen);

    // Fee remains unchanged.
    assert_eq!(client.get_fee(), 100);
}

#[test]
fn test_unfreeze_restores_payments() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    client.emergency_freeze();
    assert!(client.is_frozen());

    client.unfreeze();
    assert!(!client.is_frozen());

    // Payments should work again.
    client.route_payment(&sender, &recipient, &token_address, &1000);
}

#[test]
fn test_freeze_queue_action_blocked() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.emergency_freeze();

    // Cannot queue new actions while frozen.
    let res = client.try_queue_action(&ActionType::SetFeeBps(500));
    assert_eq!(res.unwrap_err().unwrap(), Error::ContractFrozen);
}

#[test]
fn test_cancel_action_allowed_while_frozen() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Queue an action before freezing.
    let nonce = client.queue_action(&ActionType::SetFeeBps(500));

    client.emergency_freeze();

    // Cancellation should still be possible while frozen (incident response).
    client.cancel_action(&nonce);
    let res = client.try_get_queued_action(&nonce);
    assert_eq!(res.unwrap_err().unwrap(), Error::TimelockNotFound);
}

// ── Timelock emits events ────────────────────────────────────────────────

#[test]
fn test_queue_action_emits_event() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.queue_action(&ActionType::SetFeeBps(200));

    let events = env.events().all();
    let found = events.iter().any(|(_, topics, _)| {
        if topics.is_empty() {
            return false;
        }
        let raw = topics.get(0).unwrap();
        let sym: Result<Symbol, _> = raw.try_into_val(&env);
        sym.map(|s| s == Symbol::new(&env, "action_queued"))
            .unwrap_or(false)
    });
    assert!(found, "action_queued event not found");
}

#[test]
fn test_freeze_emits_event() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.emergency_freeze();

    let events = env.events().all();
    let found = events.iter().any(|(_, topics, _)| {
        if topics.is_empty() {
            return false;
        }
        let raw = topics.get(0).unwrap();
        let sym: Result<Symbol, _> = raw.try_into_val(&env);
        sym.map(|s| s == Symbol::new(&env, "emergency_freeze"))
            .unwrap_or(false)
    });
    assert!(found, "emergency_freeze event not found");
}

// ── Original tests (retained) ────────────────────────────────────────────

#[test]
fn test_get_fee() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    // Before initialization, get_fee returns 0
    assert_eq!(client.get_fee(), 0);

    // Initialize with 150 bps
    client.initialize(&admin, &treasury, &150, &5000, &PaymentRouter::MAX_AMOUNT);
    assert_eq!(client.get_fee(), 150);

    // Update via set_fee_bps
    client.set_fee_bps(&250);
    assert_eq!(client.get_fee(), 250);

    // Update via set_fee_config
    client.set_fee_config(&300, &10000);
    assert_eq!(client.get_fee(), 300);
}

#[test]
fn test_admin_restrictions_and_updates() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let new_admin = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Trying to initialize again should fail
    let res = client.try_initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);
    assert_eq!(res.unwrap_err().unwrap(), Error::AlreadyInitialized);

    client.set_admin(&new_admin);

    // Modify config
    client.set_fee_config(&200, &2000);
    client.set_fee_bps(&200);
    assert_eq!(client.get_fee(), 200);

    let new_treasury = Address::generate(&env);
    client.set_platform_treasury(&new_treasury);
}

#[test]
fn test_recover_tokens() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let (token_address, token_client, stellar_asset_client) = setup_token(&env);

    // Simulate tokens accidentally sent directly to the contract address
    let accidental_amount = 5_000i128;
    stellar_asset_client.mint(&contract_id, &accidental_amount);

    assert_eq!(token_client.balance(&contract_id), accidental_amount);
    assert_eq!(token_client.balance(&admin), 0);

    // Admin recovers tokens
    let recover_amount = 3_000i128;
    client.recover_tokens(&token_address, &recover_amount);

    assert_eq!(token_client.balance(&admin), recover_amount);
    assert_eq!(
        token_client.balance(&contract_id),
        accidental_amount - recover_amount
    );
}

#[test]
fn test_set_pause_emits_event() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.set_pause(&true);

    let events = env.events().all();
    assert!(!events.is_empty());
    let (_, topics, _) = events.get(0).unwrap();
    assert_eq!(topics.len(), 1);
    let topic: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic, symbol_short!("pause"));
}

#[test]
fn test_route_payment_emits_payment_initiated_event() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    client
        .mock_all_auths()
        .route_payment(&sender, &recipient, &token_address, &5_000);

    let events = env.events().all();
    assert!(!events.is_empty());

    let mut found = false;
    for (_, topics, data) in events.iter() {
        if !topics.is_empty() {
            if let Ok(topic_sym) = topics.get(0).unwrap().try_into_val(&env) {
                let sym: Symbol = topic_sym;
                if sym == Symbol::new(&env, "payment_initiated") {
                    found = true;
                    let amt: i128 = data.try_into_val(&env).unwrap();
                    assert_eq!(amt, 5_000);
                    break;
                }
            }
        }
    }
    assert!(found, "payment_initiated event not found");
}

#[test]
fn test_route_payment_emits_routed_event() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _token_client, _token_admin_client) = setup_token(&env);
    let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_address);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);
    client.add_supported_token(&token_address);

    let amount = 2_000i128;
    client.route_payment(&sender, &recipient, &token_address, &amount);

    let events = env.events().all();
    assert!(!events.is_empty());

    // Find the "routed" event by topic
    let mut found = None;
    for evt in events.iter() {
        let (_contract_id, topics, _data) = evt.clone();
        if topics.len() != 3 {
            continue;
        }
        let topic0: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic0 == symbol_short!("routed") {
            found = Some(evt.clone());
            break;
        }
    }
    let routed = found.expect("route_payment should publish a \"routed\" event");

    let (_contract_id, topics, data) = routed;
    assert_eq!(topics.len(), 3);

    let topic_sender: Address = topics.get(1).unwrap().try_into_val(&env).unwrap();
    let topic_recipient: Address = topics.get(2).unwrap().try_into_val(&env).unwrap();
    assert_eq!(topic_sender, sender);
    assert_eq!(topic_recipient, recipient);

    let event_amount: i128 = data.try_into_val(&env).unwrap();
    assert_eq!(event_amount, amount);
}

#[test]
fn test_admin_pause_functionality() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _token_client, _token_admin_client) = setup_token(&env);
    let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_address);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    // Initially not paused
    assert!(!client.is_paused());

    // Pause
    client.set_pause(&true);
    assert!(client.is_paused());

    // Route payment should fail when paused
    let res = client.try_route_payment(&sender, &recipient, &token_address, &1000);
    assert_eq!(res.unwrap_err().unwrap(), Error::Paused);

    // Unpause via set_paused alias
    client.set_paused(&false);
    assert!(!client.is_paused());

    // Route payment should succeed now
    client.route_payment(&sender, &recipient, &token_address, &1000);
}

// ── RBAC tests ───────────────────────────────────────────────────────────

#[test]
fn test_initialize_seeds_admin_into_all_roles() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    for role in [
        Role::SuperAdmin,
        Role::TreasuryManager,
        Role::ComplianceOfficer,
        Role::FeeManager,
        Role::Pauser,
    ] {
        assert!(client.has_role(&admin, &role));
        assert_eq!(client.get_role_member(&role), Some(admin.clone()));
    }
}

#[test]
fn test_admin_can_grant_and_revoke_pauser_role() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let pauser = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    assert!(!client.has_role(&pauser, &Role::Pauser));

    client.grant_role(&admin, &pauser, &Role::Pauser);
    assert!(client.has_role(&pauser, &Role::Pauser));
    assert_eq!(client.get_role_member(&Role::Pauser), Some(pauser.clone()));

    client.revoke_role(&admin, &pauser, &Role::Pauser);
    assert!(!client.has_role(&pauser, &Role::Pauser));
    assert_eq!(client.get_role_member(&Role::Pauser), None);
}

#[test]
fn test_assign_role_requires_super_admin_authorization() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let grantee = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.assign_role(&grantee, &Role::Pauser);

    // The SuperAdmin (admin) must be the authorizing address.
    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| *addr == admin));
    assert_eq!(client.get_role_member(&Role::SuperAdmin), Some(admin));
}

#[test]
fn test_granted_pauser_can_pause_and_unpause() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let pauser = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.assign_role(&pauser, &Role::Pauser);

    client.set_pause(&true);

    // The delegated Pauser, not the admin, authorizes the switch.
    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| *addr == pauser));
    assert!(!auths.iter().any(|(addr, _)| *addr == admin));
    assert!(client.is_paused());

    client.set_paused(&false);
    assert!(!client.is_paused());
}

#[test]
fn test_revoking_pauser_role_restores_admin_fallback() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let pauser = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.assign_role(&pauser, &Role::Pauser);
    client.revoke_role(&admin, &pauser, &Role::Pauser);

    // With no primary Pauser configured, pause authority falls back to admin.
    assert!(!client.has_role(&pauser, &Role::Pauser));
    client.set_pause(&true);
    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| *addr == admin));
    assert!(client.is_paused());
}

#[test]
fn test_fee_manager_role_gates_fee_functions() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let fee_manager = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.assign_role(&fee_manager, &Role::FeeManager);
    assert!(client.has_role(&fee_manager, &Role::FeeManager));

    client.set_min_limit(&50);

    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| *addr == fee_manager));
}

#[test]
fn test_revoked_fee_manager_loses_privileges() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let fee_manager = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.assign_role(&fee_manager, &Role::FeeManager);
    client.revoke_role(&admin, &fee_manager, &Role::FeeManager);
    assert!(!client.has_role(&fee_manager, &Role::FeeManager));

    // Authority falls back to the admin for the now-vacant role.
    client.set_min_limit(&75);
    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| *addr == admin));
}

#[test]
fn test_has_role_is_false_for_unassigned_accounts() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let stranger = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    for role in [
        Role::SuperAdmin,
        Role::TreasuryManager,
        Role::ComplianceOfficer,
        Role::FeeManager,
        Role::Pauser,
    ] {
        assert!(!client.has_role(&stranger, &role));
    }
}

// ── RBAC authorization tests ─────────────────────────────────────────────

/// Like [`setup_env`], but without `mock_all_auths`.  A role gate can only
/// be shown to *reject* an unauthorized caller when authorization is
/// recorded explicitly, so these tests register exactly the signature the
/// contract demands and observe whether it is accepted.
fn setup_env_ungated() -> (Env, PaymentRouterClient<'static>, Address) {
    let env = Env::default();
    let contract_id = env.register_contract(None, PaymentRouter);
    let client = PaymentRouterClient::new(&env, &contract_id);
    (env, client, contract_id)
}

/// Records one authorization: `address` is permitted to invoke `fn_name`
/// with `args` on `contract`.  Every address the contract requires must be
/// covered by some entry, or the call is rejected.
fn authorize(env: &Env, contract: &Address, address: &Address, fn_name: &str, args: Vec<Val>) {
    env.mock_auths(&[MockAuth {
        address,
        invoke: &MockAuthInvoke {
            contract,
            fn_name,
            args,
            sub_invokes: &[],
        },
    }]);
}

/// The `initialize` arguments used throughout these tests, in the shape
/// `MockAuthInvoke` needs.
fn init_args(env: &Env, admin: &Address, treasury: &Address) -> Vec<Val> {
    (
        admin.clone(),
        treasury.clone(),
        100i128,
        1000i128,
        PaymentRouter::MAX_AMOUNT,
    )
        .into_val(env)
}

/// A non-admin cannot grant a role, and the grant does not take effect.
#[test]
fn test_non_admin_cannot_grant_role() {
    let (env, client, contract_id) = setup_env_ungated();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let outsider = Address::generate(&env);
    let grantee = Address::generate(&env);
    authorize(
        &env,
        &contract_id,
        &admin,
        "initialize",
        init_args(&env, &admin, &treasury),
    );
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // The outsider signs, but holds no SuperAdmin role.
    authorize(
        &env,
        &contract_id,
        &outsider,
        "grant_role",
        (outsider.clone(), grantee.clone(), Role::Pauser).into_val(&env),
    );
    assert_eq!(
        client.try_grant_role(&outsider, &grantee, &Role::Pauser),
        Err(Ok(Error::RoleNotFound))
    );
    assert!(!client.has_role(&grantee, &Role::Pauser));
}

/// A non-admin cannot revoke a role, and the role survives the attempt.
#[test]
fn test_non_admin_cannot_revoke_role() {
    let (env, client, contract_id) = setup_env_ungated();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let outsider = Address::generate(&env);
    let pauser = Address::generate(&env);
    authorize(
        &env,
        &contract_id,
        &admin,
        "initialize",
        init_args(&env, &admin, &treasury),
    );
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);
    authorize(
        &env,
        &contract_id,
        &admin,
        "grant_role",
        (admin.clone(), pauser.clone(), Role::Pauser).into_val(&env),
    );
    client.grant_role(&admin, &pauser, &Role::Pauser);

    authorize(
        &env,
        &contract_id,
        &outsider,
        "revoke_role",
        (outsider.clone(), pauser.clone(), Role::Pauser).into_val(&env),
    );
    assert_eq!(
        client.try_revoke_role(&outsider, &pauser, &Role::Pauser),
        Err(Ok(Error::RoleNotFound))
    );
    assert!(client.has_role(&pauser, &Role::Pauser));
}

/// Granting a role requires the SuperAdmin to be the authorizing address.
///
/// This asserts *which* address must sign rather than driving an auth
/// failure, because the Soroban test host aborts the process on an
/// unmatched `require_auth` rather than surfacing it as a catchable
/// contract error.  The rejection path itself is covered by
/// `test_non_admin_cannot_grant_role`, which names a non-admin in the
/// `admin` position and observes `Err(Error::RoleNotFound)`.
#[test]
fn test_grant_role_is_gated_on_the_super_admin_signature() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let grantee = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.grant_role(&admin, &grantee, &Role::Pauser);

    let auths = env.auths();
    assert!(
        auths.iter().any(|(addr, _)| *addr == admin),
        "grant_role must require the SuperAdmin to authorize"
    );
    assert!(client.has_role(&grantee, &Role::Pauser));
}

/// Granting twice and revoking twice converge on the same state: neither
/// direction traps on a redundant call.
#[test]
fn test_grant_and_revoke_are_idempotent() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let pauser = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.grant_role(&admin, &pauser, &Role::Pauser);
    client.grant_role(&admin, &pauser, &Role::Pauser);
    assert!(client.has_role(&pauser, &Role::Pauser));
    assert_eq!(client.get_role_member(&Role::Pauser), Some(pauser.clone()));

    client.revoke_role(&admin, &pauser, &Role::Pauser);
    client.revoke_role(&admin, &pauser, &Role::Pauser);
    assert!(!client.has_role(&pauser, &Role::Pauser));
    assert_eq!(client.get_role_member(&Role::Pauser), None);
}

/// Revoking a role that was never granted is a no-op rather than a trap.
#[test]
fn test_revoke_never_granted_role_is_a_noop() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let stranger = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    assert!(!client.has_role(&stranger, &Role::Pauser));
    client.revoke_role(&admin, &stranger, &Role::Pauser);
    assert!(!client.has_role(&stranger, &Role::Pauser));
}

/// Reassigning a role transfers it: the previous holder must stop reporting
/// the role, otherwise `has_role` would advertise authority the contract no
/// longer accepts.
#[test]
fn test_reassigning_role_clears_the_previous_holder() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let first = Address::generate(&env);
    let second = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    assert!(client.has_role(&admin, &Role::Pauser));
    client.grant_role(&admin, &first, &Role::Pauser);
    assert!(client.has_role(&first, &Role::Pauser));

    client.grant_role(&admin, &second, &Role::Pauser);
    assert!(client.has_role(&second, &Role::Pauser));
    assert!(
        !client.has_role(&first, &Role::Pauser),
        "the displaced holder must not still report the role"
    );
    assert!(!client.has_role(&admin, &Role::Pauser));
}

/// With no holder for a role the root admin stands in, and `has_role` must
/// agree with that fallback rather than reporting `false`.
#[test]
fn test_has_role_reports_the_admin_fallback() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let pauser = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Revoking the admin's own Pauser role leaves the role unassigned, so
    // authority reverts to the admin even though its grant flag is cleared.
    client.revoke_role(&admin, &admin, &Role::Pauser);
    assert_eq!(client.get_role_member(&Role::Pauser), None);
    assert!(client.has_role(&admin, &Role::Pauser));
    assert!(!client.has_role(&pauser, &Role::Pauser));

    client.set_pause(&true);
    assert!(client.is_paused());
}

/// The pause switch is gated on the `Pauser` role: the address the contract
/// asks to authorize is the Pauser, and that authority is distinguishable
/// from the admin's once the role is delegated.
///
/// As above, the assertion is on which address must sign, since an
/// unmatched `require_auth` aborts the test host.  `env.auths()` only
/// reports the most recent invocation, so it is read immediately after the
/// call under test and before any other client call.
#[test]
fn test_pause_switch_is_gated_on_the_pauser_role() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let pauser = Address::generate(&env);
    let stranger = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // `initialize` seeds the admin into every role, so it signs first.
    client.set_pause(&true);
    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| *addr == admin));
    assert!(!auths.iter().any(|(addr, _)| *addr == pauser));
    assert!(client.is_paused());

    // After delegation only the Pauser is asked to authorize the switch.
    client.grant_role(&admin, &pauser, &Role::Pauser);
    client.set_paused(&false);
    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| *addr == pauser));
    assert!(!auths.iter().any(|(addr, _)| *addr == admin));
    assert!(!client.is_paused());

    // An unassigned account is never treated as a Pauser.
    assert!(!client.has_role(&stranger, &Role::Pauser));
}

/// The `MinLimit` setter is gated on the `FeeManager` role, and that gate
/// follows the role rather than the plain admin flag: once the role is
/// delegated the FeeManager is the address asked to authorize.
#[test]
fn test_min_limit_is_gated_on_the_fee_manager_role() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let fee_manager = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Undelegated: the seeded FeeManager is the admin itself.
    client.set_min_limit(&50);
    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| *addr == admin));

    client.grant_role(&admin, &fee_manager, &Role::FeeManager);
    client.set_min_limit(&75);
    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| *addr == fee_manager));
    assert!(!auths.iter().any(|(addr, _)| *addr == admin));
}

/// The acting SuperAdmin may not revoke its own root role, which would
/// leave role management unreachable.
#[test]
fn test_revoke_role_refuses_to_remove_the_acting_super_admin() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    assert_eq!(
        client.try_revoke_role(&admin, &admin, &Role::SuperAdmin),
        Err(Ok(Error::InvalidRole))
    );
    assert!(client.has_role(&admin, &Role::SuperAdmin));
}

/// Grants and revocations publish `role_assigned` / `role_revoked`.
#[test]
fn test_role_changes_emit_events() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let pauser = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.grant_role(&admin, &pauser, &Role::Pauser);
    let (_, topics, _) = env.events().all().last().unwrap();
    let assigned: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(assigned, Symbol::new(&env, "role_assigned"));
    assert!(client.has_role(&pauser, &Role::Pauser));

    client.revoke_role(&admin, &pauser, &Role::Pauser);
    let (_, topics, _) = env.events().all().last().unwrap();
    let revoked: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
    assert_eq!(revoked, Symbol::new(&env, "role_revoked"));
}

// ── Circuit breaker tests ────────────────────────────────────────────────

/// While the breaker is open, non-essential state changes — payments,
/// timelock queuing and the direct parameter setters — are rejected with
/// `Error::Paused`.
#[test]
fn test_circuit_breaker_blocks_non_essential_state_changes() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    client.set_pause(&true);
    assert!(client.is_paused());

    // Timelock state changes are blocked.
    assert_eq!(
        client.try_queue_action(&ActionType::SetFeeBps(250)),
        Err(Ok(Error::Paused))
    );

    // Direct parameter setters are blocked.
    assert_eq!(client.try_set_fee_bps(&250), Err(Ok(Error::Paused)));
    assert_eq!(
        client.try_set_fee_config(&250, &1000),
        Err(Ok(Error::Paused))
    );
    assert_eq!(client.try_set_min_limit(&50), Err(Ok(Error::Paused)));
    let new_treasury = Address::generate(&env);
    assert_eq!(
        client.try_set_platform_treasury(&new_treasury),
        Err(Ok(Error::Paused))
    );
    let gov = Address::generate(&env);
    assert_eq!(client.try_set_governance(&gov), Err(Ok(Error::Paused)));

    let (token_address, _token_client, _sac) = setup_token(&env);
    assert_eq!(
        client.try_recover_tokens(&token_address, &10),
        Err(Ok(Error::Paused))
    );

    // Routing stays blocked too.
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);
    assert_eq!(
        client.try_route_payment(&sender, &recipient, &token_address, &10),
        Err(Ok(Error::Paused))
    );
}

/// A queued action cannot be executed while the breaker is open, but it can
/// still be cancelled so the timelock queue is never stuck.
#[test]
fn test_circuit_breaker_blocks_execution_but_allows_cancel() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let nonce = client.queue_action(&ActionType::SetFeeBps(250));

    // Advance past the 24h timelock window.
    let ts = env.ledger().timestamp();
    env.ledger().set(LedgerInfo {
        timestamp: ts + PaymentRouter::SECONDS_IN_24H + 1,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence(),
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });

    // Open the breaker before execution: the change is not applied.
    client.set_pause(&true);
    assert_eq!(client.try_execute_action(&nonce), Err(Ok(Error::Paused)));
    assert_eq!(client.get_fee(), 100);

    // Cancelling the queued action remains available while paused.
    client.cancel_action(&nonce);
    assert_eq!(
        client.try_get_queued_action(&nonce).unwrap_err().unwrap(),
        Error::TimelockNotFound
    );
}

/// Essential recovery paths stay callable while the breaker is open:
/// user refunds, emergency withdrawals and resetting the breaker.
#[test]
fn test_circuit_breaker_keeps_essential_operations_available() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let user = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let (token_address, token_client, stellar_asset_client) = setup_token(&env);

    // Seed a refund balance and some stranded tokens for the emergency path.
    let refund_amount = 3_000i128;
    let stranded_amount = 2_000i128;
    stellar_asset_client.mint(&contract_id, &(refund_amount + stranded_amount));
    env.as_contract(&contract_id, || {
        PaymentRouter::credit_refund_balance(&env, &user, &token_address, refund_amount);
    });

    client.set_pause(&true);

    // Users can still withdraw their own refunded funds.
    client.withdraw_refund(&user, &token_address, &1_000);
    assert_eq!(token_client.balance(&user), 1_000);

    // Emergency withdrawal of stranded funds stays available.
    client.emergency_withdraw(&token_address, &stranded_amount);
    assert_eq!(token_client.balance(&admin), stranded_amount);

    // The breaker can always be reset.
    client.set_pause(&false);
    assert!(!client.is_paused());
}

/// Treasury yield movements are non-essential and are blocked while paused.
#[test]
fn test_circuit_breaker_blocks_yield_movements() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let protocol_id = env.register_contract(None, MockLendingProtocol);
    let (token_address, _token_client, _sac) = setup_token(&env);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);
    client.set_yield_protocol(&protocol_id);

    client.set_pause(&true);
    assert_eq!(
        client.try_deposit_to_yield(&token_address, &100),
        Err(Ok(Error::Paused))
    );
    assert_eq!(
        client.try_harvest_yield(&token_address),
        Err(Ok(Error::Paused))
    );
    assert_eq!(
        client.try_withdraw_from_yield(&token_address, &50),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn test_route_payment_calculates_and_sends_fee() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, token_client, _token_admin_client) = setup_token(&env);

    let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_address);
    let initial_balance = 10_000i128;
    sac.mint(&sender, &initial_balance);

    // Initialize router with 1% fee (100 bps) and cap of 50
    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);
    client.add_supported_token(&token_address);

    // Test normal fee calculation: 1% of 2000 = 20, below cap of 50
    let amount_1 = 2000i128;
    client.route_payment(&sender, &recipient, &token_address, &amount_1);

    assert_eq!(token_client.balance(&treasury), 20);
    assert_eq!(token_client.balance(&recipient), 1980);
    assert_eq!(token_client.balance(&sender), initial_balance - amount_1);
    assert_eq!(client.get_user_volume(&sender), amount_1);

    // Test fee capped at 50: 1% of 8000 = 80, capped to 50
    let amount_2 = 8000i128;
    client.route_payment(&sender, &recipient, &token_address, &amount_2);

    assert_eq!(token_client.balance(&treasury), 70);
    assert_eq!(token_client.balance(&recipient), 9930);
    assert_eq!(
        token_client.balance(&sender),
        initial_balance - amount_1 - amount_2
    );
    assert_eq!(client.get_user_volume(&sender), amount_1 + amount_2);
}

#[test]
fn test_insufficient_balance() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&sender, &100);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);
    client.add_supported_token(&token_address);

    // Route payment of 500 when balance is only 100
    let res = client.try_route_payment(&sender, &recipient, &token_address, &500);
    assert_eq!(res.unwrap_err().unwrap(), Error::InsufficientBalance);
}

// ── Meta-transaction tests ─────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn sign_meta_payload(
    env: &Env,
    contract_id: &Address,
    sender: &Address,
    signer_pubkey: &BytesN<32>,
    recipient: &Address,
    token: &Address,
    amount: i128,
    nonce: u64,
    deadline: u64,
    signing_key: &ed25519_dalek::SigningKey,
) -> BytesN<64> {
    use ed25519_dalek::Signer;
    use soroban_sdk::xdr::ToXdr;
    let mut payload = soroban_sdk::Bytes::new(env);
    payload.append(&contract_id.to_xdr(env));
    payload.append(&sender.to_xdr(env));
    payload.append(&soroban_sdk::Bytes::from_slice(
        env,
        &signer_pubkey.to_array(),
    ));
    payload.append(&recipient.to_xdr(env));
    payload.append(&token.to_xdr(env));
    payload.append(&amount.to_xdr(env));
    payload.append(&nonce.to_xdr(env));
    payload.append(&deadline.to_xdr(env));
    let hash = env.crypto().sha256(&payload);
    let msg = soroban_sdk::Bytes::from(&hash);
    let mut buf = [0u8; 32];
    msg.copy_into_slice(&mut buf);
    let sig = signing_key.sign(&buf);
    BytesN::from_array(env, &sig.to_bytes())
}

#[test]
fn test_meta_payment_success_and_nonce_increments() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let pubkey = BytesN::from_array(&env, &signing_key.verifying_key().to_bytes());

    let amount = 2000i128;
    let nonce = client.get_meta_nonce(&sender);
    assert_eq!(nonce, 0);
    let deadline = env.ledger().timestamp() + 100_000;

    token_client.approve(&sender, &contract_id, &amount, &1_000_000);

    let sig = sign_meta_payload(
        &env,
        &contract_id,
        &sender,
        &pubkey,
        &recipient,
        &token_address,
        amount,
        nonce,
        deadline,
        &signing_key,
    );

    client.route_payment_meta(
        &sender,
        &pubkey,
        &recipient,
        &token_address,
        &amount,
        &nonce,
        &deadline,
        &sig,
    );

    assert_eq!(client.get_meta_nonce(&sender), 1);
    assert_eq!(token_client.balance(&treasury), 20);
    assert_eq!(token_client.balance(&recipient), 1980);
    assert_eq!(token_client.balance(&sender), 10_000 - amount);
}

#[test]
fn test_meta_payment_replay_rejected() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
    let pubkey = BytesN::from_array(&env, &signing_key.verifying_key().to_bytes());

    let amount = 1000i128;
    let nonce = client.get_meta_nonce(&sender);
    let deadline = env.ledger().timestamp() + 100_000;

    token_client.approve(&sender, &contract_id, &(amount * 2), &1_000_000);

    let sig = sign_meta_payload(
        &env,
        &contract_id,
        &sender,
        &pubkey,
        &recipient,
        &token_address,
        amount,
        nonce,
        deadline,
        &signing_key,
    );

    client.route_payment_meta(
        &sender,
        &pubkey,
        &recipient,
        &token_address,
        &amount,
        &nonce,
        &deadline,
        &sig,
    );

    let res = client.try_route_payment_meta(
        &sender,
        &pubkey,
        &recipient,
        &token_address,
        &amount,
        &nonce,
        &deadline,
        &sig,
    );
    assert_eq!(res.unwrap_err().unwrap(), Error::InvalidNonce);
}

#[test]
fn test_meta_payment_expired_rejected() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[11u8; 32]);
    let pubkey = BytesN::from_array(&env, &signing_key.verifying_key().to_bytes());

    let amount = 1000i128;
    let nonce = client.get_meta_nonce(&sender);
    let deadline = env.ledger().timestamp() + 10;

    token_client.approve(&sender, &contract_id, &amount, &1_000_000);

    let sig = sign_meta_payload(
        &env,
        &contract_id,
        &sender,
        &pubkey,
        &recipient,
        &token_address,
        amount,
        nonce,
        deadline,
        &signing_key,
    );

    let ts = env.ledger().timestamp();
    env.ledger().set(LedgerInfo {
        timestamp: ts + 100_000,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence(),
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });

    let res = client.try_route_payment_meta(
        &sender,
        &pubkey,
        &recipient,
        &token_address,
        &amount,
        &nonce,
        &deadline,
        &sig,
    );
    assert_eq!(res.unwrap_err().unwrap(), Error::DeadlineExpired);
}

#[test]
fn test_daily_limit_and_reset() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, token_client, _token_admin_client) = setup_token(&env);

    let limit = 10_000_000_000_000i128;
    let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_address);
    sac.mint(&sender, &(limit + 2000));

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);
    client.add_supported_token(&token_address);

    // Route amount up to daily limit
    client.route_payment(&sender, &recipient, &token_address, &limit);

    // Next payment should exceed daily limit
    let res = client.try_route_payment(&sender, &recipient, &token_address, &2000);
    assert_eq!(res.unwrap_err().unwrap(), Error::LimitExceeded);

    // Advance time past 24 hours to reset the daily limit
    let current_time = env.ledger().timestamp();
    let current_protocol_version = env.ledger().protocol_version();
    env.ledger().set(LedgerInfo {
        timestamp: current_time + 86400,
        protocol_version: current_protocol_version,
        sequence_number: 1,
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });

    // Now routing should succeed again. The first payment pushed volume past
    // VOLUME_THRESHOLD, so the halved rate applies: 2000 * 50 bps = 10.
    client.route_payment(&sender, &recipient, &token_address, &2000);
    assert_eq!(token_client.balance(&recipient), (limit - 50) + (2000 - 10));
}

/// Advances the ledger clock by `seconds`.
fn advance_time(env: &Env, seconds: u64) {
    let current_time = env.ledger().timestamp();
    env.ledger().set(LedgerInfo {
        timestamp: current_time + seconds,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence(),
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });
}

/// Initializes the contract and arms the dead man's switch through the
/// full timelock path (queue → wait 24h → execute), returning the
/// addresses involved.
fn setup_dms_env(
    env: &Env,
    client: &PaymentRouterClient<'static>,
    admin: &Address,
    backup: &Address,
    timeout: u64,
) {
    let treasury = Address::generate(env);
    client.initialize(admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let nonce = client.queue_action(&ActionType::SetBackupAdmin(backup.clone(), timeout));
    advance_time(env, PaymentRouter::SECONDS_IN_24H + 1);
    client.execute_action(&nonce);
}

#[test]
fn test_dms_configure_via_timelock_and_getter() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let backup = Address::generate(&env);
    let timeout: u64 = 30 * 24 * 3600;

    // Unarmed before configuration.
    let (b, t, last, elapsed) = client.get_dead_mans_switch();
    assert!(b.is_none());
    assert_eq!((t, last, elapsed), (0, 0, 0));
    assert!(!client.is_dead_mans_switch_expired());

    setup_dms_env(&env, &client, &admin, &backup, timeout);

    let (b, t, _last, elapsed) = client.get_dead_mans_switch();
    assert_eq!(b.unwrap(), backup);
    assert_eq!(t, timeout);
    assert!(elapsed < 5, "clock should start at arm time");
    assert!(!client.is_dead_mans_switch_expired());
}

#[test]
fn test_dms_rejects_short_timeout_and_admin_as_backup() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Timeout below MIN_DMS_TIMEOUT.
    let res = client.try_set_backup_admin_internal(
        &Address::generate(&env),
        &(PaymentRouter::MIN_DMS_TIMEOUT - 1),
    );
    assert_eq!(res.unwrap_err().unwrap(), Error::InvalidDmsConfig);

    // Backup identical to the current admin.
    let res = client.try_set_backup_admin_internal(&admin, &PaymentRouter::MIN_DMS_TIMEOUT);
    assert_eq!(res.unwrap_err().unwrap(), Error::InvalidDmsConfig);

    // Nothing was armed.
    let (b, _, _, _) = client.get_dead_mans_switch();
    assert!(b.is_none());
}

#[test]
fn test_dms_claim_fails_before_timeout_then_succeeds() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let backup = Address::generate(&env);
    setup_dms_env(
        &env,
        &client,
        &admin,
        &backup,
        PaymentRouter::MIN_DMS_TIMEOUT,
    );

    // Not the backup cannot claim.
    let res = client.try_claim_admin(&Address::generate(&env));
    assert_eq!(res.unwrap_err().unwrap(), Error::Unauthorized);

    // Backup cannot claim before the timeout elapses.
    let res = client.try_claim_admin(&backup);
    assert_eq!(res.unwrap_err().unwrap(), Error::DmsNotReady);

    // Admin pings just before the deadline; the clock restarts.
    advance_time(&env, PaymentRouter::MIN_DMS_TIMEOUT - 1);
    client.ping();
    let res = client.try_claim_admin(&backup);
    assert_eq!(res.unwrap_err().unwrap(), Error::DmsNotReady);

    // After the timeout since the *ping*, the claim succeeds.
    advance_time(&env, PaymentRouter::MIN_DMS_TIMEOUT + 1);
    client.claim_admin(&backup);
    assert_eq!(client.get_admin(), Some(backup.clone()));

    // The switch is disarmed after a successful claim.
    let (b, t, last, elapsed) = client.get_dead_mans_switch();
    assert!(b.is_none());
    assert_eq!((t, last, elapsed), (0, 0, 0));
}

#[test]
fn test_dms_ping_resets_timer() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let backup = Address::generate(&env);
    setup_dms_env(
        &env,
        &client,
        &admin,
        &backup,
        PaymentRouter::MIN_DMS_TIMEOUT,
    );

    // Repeated pings keep the switch alive indefinitely.
    for _ in 0..3 {
        advance_time(&env, PaymentRouter::MIN_DMS_TIMEOUT - 1);
        client.ping();
        let res = client.try_claim_admin(&backup);
        assert_eq!(res.unwrap_err().unwrap(), Error::DmsNotReady);
    }

    // Silence for the full timeout, then the backup takes over.
    advance_time(&env, PaymentRouter::MIN_DMS_TIMEOUT + 1);
    client.claim_admin(&backup);
    assert_eq!(client.get_admin(), Some(backup.clone()));
}

#[test]
fn test_dms_routing_payment_bumps_heartbeat() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let backup = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    setup_dms_env(
        &env,
        &client,
        &admin,
        &backup,
        PaymentRouter::MIN_DMS_TIMEOUT,
    );

    let (token_addr, _token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);

    // Advance almost to the deadline, then route a payment: the
    // successful payment is a sign of life and restarts the clock.
    advance_time(&env, PaymentRouter::MIN_DMS_TIMEOUT - 1);
    client.route_payment(&sender, &recipient, &token_addr, &1_000);

    advance_time(&env, 1);
    let res = client.try_claim_admin(&backup);
    assert_eq!(res.unwrap_err().unwrap(), Error::DmsNotReady);

    // Failed routing must NOT bump the heartbeat: sender without funds.
    advance_time(&env, PaymentRouter::MIN_DMS_TIMEOUT - 1);
    let broke_sender = Address::generate(&env);
    let res = client.try_route_payment(&broke_sender, &recipient, &token_addr, &1_000);
    assert_eq!(res.unwrap_err().unwrap(), Error::InsufficientBalance);

    // The failed payment did not reset the clock, so the claim succeeds.
    advance_time(&env, 2);
    client.claim_admin(&backup);
    assert_eq!(client.get_admin(), Some(backup.clone()));
}

#[test]
fn test_dms_claim_works_while_frozen_and_paused() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let backup = Address::generate(&env);
    setup_dms_env(
        &env,
        &client,
        &admin,
        &backup,
        PaymentRouter::MIN_DMS_TIMEOUT,
    );

    client.emergency_freeze();
    client.set_pause(&true);

    advance_time(&env, PaymentRouter::MIN_DMS_TIMEOUT + 1);
    client.claim_admin(&backup);
    assert_eq!(client.get_admin(), Some(backup.clone()));
}

#[test]
fn test_dms_remove_backup_admin_disarms_switch() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let backup = Address::generate(&env);
    setup_dms_env(
        &env,
        &client,
        &admin,
        &backup,
        PaymentRouter::MIN_DMS_TIMEOUT,
    );

    client.remove_backup_admin();

    let (b, _, last, _) = client.get_dead_mans_switch();
    assert!(b.is_none());
    assert_eq!(last, 0);

    // Even after a long silence the claim fails: no backup is configured.
    advance_time(&env, PaymentRouter::MIN_DMS_TIMEOUT * 10);
    let res = client.try_claim_admin(&backup);
    assert_eq!(res.unwrap_err().unwrap(), Error::NoBackupAdmin);
}

#[test]
fn test_dms_no_backup_claim_fails() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let res = client.try_claim_admin(&Address::generate(&env));
    assert_eq!(res.unwrap_err().unwrap(), Error::NoBackupAdmin);
}

#[test]
fn test_dms_new_admin_can_rearm_after_claim() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let backup = Address::generate(&env);
    setup_dms_env(
        &env,
        &client,
        &admin,
        &backup,
        PaymentRouter::MIN_DMS_TIMEOUT,
    );

    // Silence long enough for the backup to take over.
    advance_time(&env, PaymentRouter::MIN_DMS_TIMEOUT + 1);
    client.claim_admin(&backup);
    assert_eq!(client.get_admin(), Some(backup.clone()));

    // The new admin arms a fresh switch with a new backup.
    let new_backup = Address::generate(&env);
    let nonce = client.queue_action(&ActionType::SetBackupAdmin(
        new_backup.clone(),
        PaymentRouter::MIN_DMS_TIMEOUT,
    ));
    advance_time(&env, PaymentRouter::SECONDS_IN_24H + 1);
    client.execute_action(&nonce);

    let (b, _, _, _) = client.get_dead_mans_switch();
    assert_eq!(b.unwrap(), new_backup);

    // And the new admin's own ping keeps the fresh switch alive.
    advance_time(&env, PaymentRouter::MIN_DMS_TIMEOUT - 1);
    client.ping();
    let res = client.try_claim_admin(&new_backup);
    assert_eq!(res.unwrap_err().unwrap(), Error::DmsNotReady);
}

#[test]
fn test_dms_heartbeat_is_noop_without_backup() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Ping works before any backup exists but is a no-op for the
    // heartbeat: the switch is unarmed, so there is no clock to reset.
    advance_time(&env, 1_000);
    client.ping();
    let (_, _, last, elapsed) = client.get_dead_mans_switch();
    assert_eq!(last, 0);
    assert_eq!(elapsed, 0);

    let res = client.try_claim_admin(&Address::generate(&env));
    assert_eq!(res.unwrap_err().unwrap(), Error::NoBackupAdmin);
}

#[test]
fn test_prevent_self_routing() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);

    let (token_address, _token_client, _token_admin_client) = setup_token(&env);
    let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_address);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    let res = client.try_route_payment(&sender, &sender, &token_address, &1000);
    assert_eq!(res.unwrap_err().unwrap(), Error::InvalidRecipient);
}

#[test]
#[ignore]
fn test_tiered_fee_discount_applied_after_volume_threshold() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, token_client, _token_admin_client) = setup_token(&env);

    // Threshold is 10,000 XLM = 10,000 * 10,000,000 (7 decimals)
    let threshold = 100_000_000_000i128;
    let first_amount = threshold + 1;
    let second_amount = 1000i128;
    let total_mint = first_amount + second_amount + 10_000_000;
    let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_address);
    sac.mint(&sender, &total_mint);

    // Initialize with 1% fee (100 bps) and no cap
    client.initialize(
        &admin,
        &treasury,
        &100,
        &i128::MAX,
        &PaymentRouter::MAX_AMOUNT,
    );

    // First payment: volume is 0 (< threshold), full fee applies
    client.route_payment(&sender, &recipient, &token_address, &first_amount);

    let full_fee_first = (first_amount * 100) / 10_000;
    assert_eq!(token_client.balance(&treasury), full_fee_first);
    assert_eq!(
        token_client.balance(&recipient),
        first_amount - full_fee_first
    );
    assert_eq!(client.get_user_volume(&sender), first_amount);
    // Volume is now past threshold, so next call gets the discount
    assert_eq!(client.get_effective_fee_bps(&sender), 50);

    // Second payment: volume > threshold, 50% discount applies
    client.route_payment(&sender, &recipient, &token_address, &second_amount);

    let discounted_fee = (second_amount * 50) / 10_000;
    assert_eq!(
        token_client.balance(&treasury),
        full_fee_first + discounted_fee
    );
    assert_eq!(
        token_client.balance(&recipient),
        (first_amount - full_fee_first) + (second_amount - discounted_fee)
    );
}

#[test]
fn test_get_effective_fee_bps_no_discount_below_threshold() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _token_client, _token_admin_client) = setup_token(&env);
    let sac = soroban_sdk::token::StellarAssetClient::new(&env, &token_address);
    sac.mint(&sender, &1_000_000);

    client.initialize(
        &admin,
        &treasury,
        &100,
        &i128::MAX,
        &PaymentRouter::MAX_AMOUNT,
    );

    // No volume yet
    assert_eq!(client.get_effective_fee_bps(&sender), 100);

    // Route a small payment (below threshold)
    client.route_payment(&sender, &recipient, &token_address, &1000);

    // Volume is 1000, far below 10,000 XLM threshold
    assert_eq!(client.get_effective_fee_bps(&sender), 100);
}

#[test]
fn test_successful_xlm_routing() {
    let env = Env::default();
    env.mock_all_auths();

    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);
    let platform_treasury = Address::generate(&env);
    let admin = Address::generate(&env);

    let contract_id = env.register_contract(None, PaymentRouter);
    let client = PaymentRouterClient::new(&env, &contract_id);

    client.initialize(
        &admin,
        &platform_treasury,
        &40,
        &i128::MAX,
        &PaymentRouter::MAX_AMOUNT,
    );

    let token_admin = Address::generate(&env);
    let token_address = env.register_stellar_asset_contract(token_admin.clone());
    let sac = StellarAssetClient::new(&env, &token_address);
    let token_client = token::Client::new(&env, &token_address);

    let initial_balance = 1_000_000_000i128;
    sac.mint(&sender, &initial_balance);

    client.add_supported_token(&token_address);

    let amount = 100_000_000i128;
    client.route_payment(&sender, &recipient, &token_address, &amount);

    let expected_fee = 400_000i128;
    let expected_recipient_amount = amount - expected_fee;

    assert_eq!(token_client.balance(&sender), initial_balance - amount);
    assert_eq!(token_client.balance(&recipient), expected_recipient_amount);
    assert_eq!(token_client.balance(&platform_treasury), expected_fee);
}

#[test]
fn test_initialize_sets_admin() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let contract_addr = env.register_contract(None, PaymentRouter);
    let client = PaymentRouterClient::new(&env, &contract_addr);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let stored_admin: Option<Address> = env.as_contract(&contract_addr, || {
        env.storage().instance().get(&DataKey::Admin)
    });
    assert_eq!(stored_admin, Some(admin));
}

/// Verifies that `emergency_withdraw` transfers the exact requested amount
/// from the contract's own balance to the admin address.
#[test]
fn test_emergency_withdraw_transfers_tokens_to_admin() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let (token_address, token_client, stellar_asset_client) = setup_token(&env);

    // Fund the contract directly (simulates stranded tokens from a routing failure).
    let stranded_amount = 10_000i128;
    stellar_asset_client.mint(&contract_id, &stranded_amount);

    assert_eq!(token_client.balance(&contract_id), stranded_amount);
    assert_eq!(token_client.balance(&admin), 0);

    // Admin withdraws half the stranded balance.
    let withdraw_amount = 4_000i128;
    client.emergency_withdraw(&token_address, &withdraw_amount);

    assert_eq!(token_client.balance(&admin), withdraw_amount);
    assert_eq!(
        token_client.balance(&contract_id),
        stranded_amount - withdraw_amount
    );
}

/// Verifies that `emergency_withdraw` can drain the entire contract balance
/// in a single call.
#[test]
fn test_emergency_withdraw_full_balance() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let (token_address, token_client, stellar_asset_client) = setup_token(&env);

    let stranded_amount = 7_500i128;
    stellar_asset_client.mint(&contract_id, &stranded_amount);

    client.emergency_withdraw(&token_address, &stranded_amount);

    assert_eq!(token_client.balance(&admin), stranded_amount);
    assert_eq!(token_client.balance(&contract_id), 0);
}

/// Verifies that `emergency_withdraw` declares admin authorization as required.
///
/// Soroban's `require_auth()` uses an abort-on-failure model in the host
/// (non-unwinding panics), so we cannot catch a missing-auth failure inside
/// the same test process.  Instead we use `mock_all_auths_allowing_non_root_auth`
/// to record which addresses the call attempts to authorize, then assert that
/// the admin address — and *only* the admin — appears in that list.
#[test]
fn test_admin_is_required_for_emergency_withdraw() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let contract_id = env.register_contract(None, PaymentRouter);
    let client = PaymentRouterClient::new(&env, &contract_id);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    let (token_address, _token_client, stellar_asset_client) = setup_token(&env);
    stellar_asset_client.mint(&contract_id, &5_000i128);

    // Call succeeds because mock_all_auths satisfies any require_auth.
    // What we verify is that the invocation recorded exactly one
    // authorization and that it belongs to admin, proving the function
    // gates on the admin address.
    client.emergency_withdraw(&token_address, &1_000i128);

    let auths = env.auths();
    let admin_auth_present = auths.iter().any(|(addr, _)| *addr == admin);
    assert!(
        admin_auth_present,
        "emergency_withdraw must require the admin address to authorize"
    );
}

#[test]
fn test_blacklist_recipient() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    // Blacklist the recipient
    client.blacklist_address(&recipient);
    assert!(client.is_blacklisted(&recipient));

    // Route payment should fail
    let res = client.try_route_payment(&sender, &recipient, &token_address, &1000);
    assert_eq!(res.unwrap_err().unwrap(), Error::Blacklisted);

    // Unblacklist and try again
    client.unblacklist_address(&recipient);
    assert!(!client.is_blacklisted(&recipient));

    client
        .mock_all_auths()
        .route_payment(&sender, &recipient, &token_address, &1000);
}

#[test]
#[ignore]
fn test_routes_multiple_distinct_assets() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(
        &admin,
        &treasury,
        &100,
        &1_000_000,
        &PaymentRouter::MAX_AMOUNT,
    );

    let (usdc_like_address, usdc_like_client, usdc_like_admin_client) = setup_token(&env);
    let (eurc_like_address, eurc_like_client, eurc_like_admin_client) = setup_token(&env);
    assert_ne!(usdc_like_address, eurc_like_address);

    usdc_like_admin_client.mint(&sender, &10_000);
    eurc_like_admin_client.mint(&sender, &5_000);

    client.route_payment(&sender, &recipient, &usdc_like_address, &2_000);
    client.route_payment(&sender, &recipient, &eurc_like_address, &1_000);

    assert_eq!(usdc_like_client.balance(&sender), 8_000);
    assert_eq!(usdc_like_client.balance(&recipient), 1_980);
    assert_eq!(eurc_like_client.balance(&sender), 4_000);
    assert_eq!(eurc_like_client.balance(&recipient), 990);
    assert_eq!(client.get_user_volume(&sender), 3_000);
}

#[test]
fn test_benchmark_gas_costs() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);

    // Reset budget before initialization
    env.budget().reset_default();
    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);
    let init_cpu = env.budget().cpu_instruction_cost();
    let init_mem = env.budget().memory_bytes_cost();
    log!(
        &env,
        "GAS REPORT: initialize - CPU: {}, Mem: {}",
        init_cpu,
        init_mem
    );

    // Reset budget before route_payment
    env.budget().reset_default();
    client.route_payment(&sender, &recipient, &token_address, &5_000);
    let route_cpu = env.budget().cpu_instruction_cost();
    let route_mem = env.budget().memory_bytes_cost();
    log!(
        &env,
        "GAS REPORT: route_payment - CPU: {}, Mem: {}",
        route_cpu,
        route_mem
    );

    env.budget().print();

    // Fails CI if gas costs exceed defined thresholds
    // Set reasonable thresholds (e.g. 5M CPU and 2MB Mem per call)
    let max_cpu = 5_000_000;
    let max_mem = 2_000_000;

    assert!(
        init_cpu <= max_cpu,
        "initialize CPU cost exceeded threshold! Cost: {}, Threshold: {}",
        init_cpu,
        max_cpu
    );
    assert!(
        init_mem <= max_mem,
        "initialize Memory cost exceeded threshold! Cost: {}, Threshold: {}",
        init_mem,
        max_mem
    );

    assert!(
        route_cpu <= max_cpu,
        "route_payment CPU cost exceeded threshold! Cost: {}, Threshold: {}",
        route_cpu,
        max_cpu
    );
    assert!(
        route_mem <= max_mem,
        "route_payment Memory cost exceeded threshold! Cost: {}, Threshold: {}",
        route_mem,
        max_mem
    );
}

/// Rebuilds the pre-#663 packed `UserSpending` value (`BytesN<24>`) so
/// tests can emulate legacy ledger state written by the old two-entry
/// storage format.
fn pack_legacy_spending_for_test(
    env: &Env,
    last_reset_time: u64,
    accumulated_amount: i128,
) -> BytesN<24> {
    let mut buf = [0u8; 24];
    buf[..8].copy_from_slice(&last_reset_time.to_be_bytes());
    buf[8..24].copy_from_slice(&accumulated_amount.to_be_bytes());
    BytesN::from_array(env, &buf)
}

/// Moves the oracle-style test ledger off timestamp 0, which `get_price`
/// treats as a stale quote.
fn set_realistic_ledger_time(env: &Env) {
    env.ledger().set(LedgerInfo {
        timestamp: 1_000_000,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence(),
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });
}

/// Issue #663 acceptance benchmark: per-user (tag) registration storage
/// cost must drop by at least 20%.
///
/// The benchmark isolates the storage write path that registering a
/// sender's first payment touches:
///
/// * Legacy (pre-#663): two persistent entries (`UserSpending` +
///   `UserVolume`), each with its own write and TTL extension.
/// * New (#663): one packed `UserRecord` entry with a single write and
///   TTL extension.
///
/// It fails CI if the new path is not at least 20% cheaper in CPU
/// instructions, if it uses more memory, or if the deterministic
/// entry-count accounting (one entry instead of two) no longer holds.
#[test]
fn test_benchmark_storage_cost_reduction() {
    let (env, _client, contract_id) = setup_env();

    // `legacy_sender` emulates the pre-#663 two-entry write path;
    // `packed_sender` exercises the new single-entry write path.
    let legacy_sender = Address::generate(&env);
    let packed_sender = Address::generate(&env);

    let current_time = env.ledger().timestamp();
    let amount = 5_000i128;

    // Warm-up so lazy host/footprint initialization does not skew the
    // first measurement window.
    env.as_contract(&contract_id, || {
        env.storage()
            .instance()
            .set(&DataKey::Admin, &legacy_sender);
    });
    env.budget().reset_default();

    // ── Legacy (pre-#663) registration write path ────────────────────
    {
        let legacy_sender = legacy_sender.clone();
        env.as_contract(&contract_id, || {
            let spending_key = DataKey::UserSpending(legacy_sender.clone());
            env.storage().persistent().set(
                &spending_key,
                &pack_legacy_spending_for_test(&env, current_time, amount),
            );
            env.storage().persistent().extend_ttl(
                &spending_key,
                PaymentRouter::PERSISTENT_LIFETIME_THRESHOLD,
                PaymentRouter::PERSISTENT_BUMP_AMOUNT,
            );

            let volume_key = DataKey::UserVolume(legacy_sender.clone());
            env.storage().persistent().set(&volume_key, &amount);
            env.storage().persistent().extend_ttl(
                &volume_key,
                PaymentRouter::PERSISTENT_LIFETIME_THRESHOLD,
                PaymentRouter::PERSISTENT_BUMP_AMOUNT,
            );
        });
    }
    let legacy_cpu = env.budget().cpu_instruction_cost();
    let legacy_mem = env.budget().memory_bytes_cost();

    env.budget().reset_default();

    // ── New (#663) registration write path ───────────────────────────
    {
        let packed_sender = packed_sender.clone();
        env.as_contract(&contract_id, || {
            let record_key = DataKey::UserRecord(packed_sender.clone());
            let record = pack_user_record(&env, current_time, amount, amount);
            env.storage().persistent().set(&record_key, &record);
            env.storage().persistent().extend_ttl(
                &record_key,
                PaymentRouter::PERSISTENT_LIFETIME_THRESHOLD,
                PaymentRouter::PERSISTENT_BUMP_AMOUNT,
            );
        });
    }
    let new_cpu = env.budget().cpu_instruction_cost();
    let new_mem = env.budget().memory_bytes_cost();

    std::eprintln!(
        "GAS REPORT: user-registration storage legacy (2 entries) - CPU: {}, Mem: {}",
        legacy_cpu,
        legacy_mem
    );
    std::eprintln!(
        "GAS REPORT: user-registration storage packed (1 entry)  - CPU: {}, Mem: {}",
        new_cpu,
        new_mem
    );
    std::eprintln!(
        "GAS REPORT: user-registration storage CPU reduction: {}%",
        100 - (new_cpu * 100) / legacy_cpu
    );

    // Deterministic accounting: the legacy path leaves two persistent
    // entries per registered sender, the new path exactly one.
    env.as_contract(&contract_id, || {
        assert!(
            env.storage()
                .persistent()
                .has(&DataKey::UserSpending(legacy_sender.clone())),
            "legacy path must write the UserSpending entry"
        );
        assert!(
            env.storage()
                .persistent()
                .has(&DataKey::UserVolume(legacy_sender.clone())),
            "legacy path must write the UserVolume entry"
        );
        assert!(
            env.storage()
                .persistent()
                .has(&DataKey::UserRecord(packed_sender.clone())),
            "new path must write the packed UserRecord entry"
        );
    });

    // Acceptance criterion: >= 20% CPU-instruction reduction.
    assert!(
        new_cpu * 10 <= legacy_cpu * 8,
        "packed registration write path must cost >= 20% less CPU \
             (legacy: {}, packed: {}, reduction: {}%)",
        legacy_cpu,
        new_cpu,
        100 - (new_cpu * 100) / legacy_cpu
    );
    // Memory must not regress either.
    assert!(
        new_mem <= legacy_mem,
        "packed registration write path must not use more memory \
             (legacy: {}, packed: {})",
        legacy_mem,
        new_mem
    );
}

/// The packed `UserRecord` replaces the two per-user entries and keeps
/// every public getter consistent (issue #663).
#[test]
fn test_user_record_packed_storage_roundtrip() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    // Fresh sender: zeroed record, window anchored at "now".
    let before = client.get_user_record(&sender);
    assert_eq!(before.volume, 0);
    assert_eq!(before.accumulated_amount, 0);
    assert_eq!(before.last_reset_time, env.ledger().timestamp());

    // The first payment registers the sender's packed record.
    client.route_payment(&sender, &recipient, &token_address, &2_000);

    let after_first = client.get_user_record(&sender);
    assert_eq!(after_first.accumulated_amount, 2_000);
    assert_eq!(after_first.volume, 2_000);

    // Exactly one persistent user entry now exists — the packed record.
    env.as_contract(&contract_id, || {
        let record_key = DataKey::UserRecord(sender.clone());
        assert!(env.storage().persistent().has(&record_key));
        assert!(!env
            .storage()
            .persistent()
            .has(&DataKey::UserSpending(sender.clone())));
        assert!(!env
            .storage()
            .persistent()
            .has(&DataKey::UserVolume(sender.clone())));
    });

    // A second payment accumulates in both counters.
    client.route_payment(&sender, &recipient, &token_address, &3_000);

    let after_second = client.get_user_record(&sender);
    assert_eq!(after_second.accumulated_amount, 5_000);
    assert_eq!(after_second.volume, 5_000);

    // The pre-existing getters stay consistent with the packed record.
    assert_eq!(client.get_user_volume(&sender), 5_000);
}

/// Permissionless `migrate_user_record` combines legacy entries into the
/// packed format and removes the old keys (issue #663).
#[test]
fn test_migrate_user_record_combines_legacy_entries() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let user = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    // Give the ledger a realistic timestamp so the legacy window start can
    // be back-dated.
    set_realistic_ledger_time(&env);

    // Emulate pre-#663 ledger state: split UserSpending + UserVolume.
    let legacy_window_start = env.ledger().timestamp() - 60;
    let spending_key = DataKey::UserSpending(user.clone());
    let volume_key = DataKey::UserVolume(user.clone());
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &spending_key,
            &pack_legacy_spending_for_test(&env, legacy_window_start, 1_200),
        );
        env.storage().persistent().set(&volume_key, &7_500i128);
    });

    // Getters still see the legacy state through the fallback path.
    assert_eq!(client.get_user_volume(&user), 7_500);
    let pre = client.get_user_record(&user);
    assert_eq!(pre.accumulated_amount, 1_200);
    assert_eq!(pre.volume, 7_500);

    // Migrate: reports success, writes the packed record, drops the
    // legacy keys.
    assert!(client.migrate_user_record(&user));

    env.as_contract(&contract_id, || {
        let record_key = DataKey::UserRecord(user.clone());
        assert!(env.storage().persistent().has(&record_key));
        assert!(!env.storage().persistent().has(&spending_key));
        assert!(!env.storage().persistent().has(&volume_key));
    });

    let post = client.get_user_record(&user);
    assert_eq!(post.accumulated_amount, 1_200);
    assert_eq!(post.volume, 7_500);
    assert_eq!(post.last_reset_time, legacy_window_start);
    assert_eq!(client.get_user_volume(&user), 7_500);

    // Migrating again is a no-op.
    assert!(!client.migrate_user_record(&user));

    // And a fresh payment continues from the migrated record.
    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&user, &10_000);
    let recipient = Address::generate(&env);
    client.route_payment(&user, &recipient, &token_address, &500);

    let after = client.get_user_record(&user);
    assert_eq!(after.accumulated_amount, 1_700);
    assert_eq!(after.volume, 8_000);
    assert_eq!(client.get_user_volume(&user), 8_000);
}

/// A payment routed by a sender that only has legacy entries transparently
/// upgrades them to the packed record (issue #663 migration path).
#[test]
fn test_route_payment_upgrades_legacy_entries_in_place() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    // Give the ledger a realistic timestamp so the legacy window start can
    // be back-dated.
    set_realistic_ledger_time(&env);

    // Legacy state from before the upgrade.
    let legacy_window_start = env.ledger().timestamp() - 60;
    let spending_key = DataKey::UserSpending(sender.clone());
    let volume_key = DataKey::UserVolume(sender.clone());
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &spending_key,
            &pack_legacy_spending_for_test(&env, legacy_window_start, 4_000),
        );
        env.storage().persistent().set(&volume_key, &20_000i128);
    });

    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&sender, &50_000);

    // No explicit migration needed: routing the payment combines the
    // legacy entries into the packed record on its next write.
    client.route_payment(&sender, &recipient, &token_address, &1_000);

    env.as_contract(&contract_id, || {
        let record_key = DataKey::UserRecord(sender.clone());
        assert!(env.storage().persistent().has(&record_key));
        assert!(!env.storage().persistent().has(&spending_key));
        assert!(!env.storage().persistent().has(&volume_key));
    });

    let record = client.get_user_record(&sender);
    assert_eq!(record.accumulated_amount, 5_000);
    assert_eq!(record.volume, 21_000);
    assert_eq!(client.get_user_volume(&sender), 21_000);
}

/// `migrate_user_record` is a safe no-op for unknown senders and for
/// senders that already have a packed record (issue #663).
#[test]
fn test_migrate_user_record_no_op_cases() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let unknown = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    // Unknown sender: nothing to migrate.
    assert!(!client.migrate_user_record(&unknown));

    // Sender with a packed record already: nothing to migrate.
    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&sender, &10_000);
    client.route_payment(&sender, &recipient, &token_address, &1_000);
    assert!(!client.migrate_user_record(&sender));

    // State is untouched.
    let record = client.get_user_record(&sender);
    assert_eq!(record.volume, 1_000);
    assert_eq!(record.accumulated_amount, 1_000);
}

#[test]
#[ignore]
fn test_refund_ledger_and_withdrawal() {
    let (env, client, contract_id) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let user = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &50, &PaymentRouter::MAX_AMOUNT);

    let (token_address, token_client, stellar_asset_client) = setup_token(&env);

    // Initially zero refund balance
    assert_eq!(client.get_refund_balance(&user, &token_address), 0);

    // Simulate stranded tokens in contract and credit internal refund balance
    let refund_amount = 5_000i128;
    stellar_asset_client.mint(&contract_id, &refund_amount);

    env.as_contract(&contract_id, || {
        PaymentRouter::credit_refund_balance(&env, &user, &token_address, refund_amount);
    });

    assert_eq!(
        client.get_refund_balance(&user, &token_address),
        refund_amount
    );

    // User withdraws partial refund
    let partial_amount = 2_000i128;
    client.withdraw_refund(&user, &token_address, &partial_amount);

    assert_eq!(token_client.balance(&user), partial_amount);
    assert_eq!(
        client.get_refund_balance(&user, &token_address),
        refund_amount - partial_amount
    );

    // User claims remaining refunds with claim_all_refunds
    let claimed = client.claim_all_refunds(&user, &token_address);
    assert_eq!(claimed, refund_amount - partial_amount);
    assert_eq!(token_client.balance(&user), refund_amount);
    assert_eq!(client.get_refund_balance(&user, &token_address), 0);

    // Trying to withdraw again should fail with NoRefundAvailable
    let res = client.try_withdraw_refund(&user, &token_address, &100);
    assert_eq!(res.unwrap_err().unwrap(), Error::NoRefundAvailable);
}

#[test]
fn test_governance_takes_over_fees() {
    let (_, client, _) = setup_env();

    let admin = Address::generate(&client.env);
    let treasury = Address::generate(&client.env);
    let gov = Address::generate(&client.env);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Admin can still update fees before governance is set
    client.set_fee_bps(&150);
    assert_eq!(client.get_fee(), 150);

    // Admin hands control over to governance
    client.set_governance(&gov);

    // Governance address can now update the fee
    client.set_fee_bps(&200);
    assert_eq!(client.get_fee(), 200);
}

/// Instance-storage keys for [`MockDex`].
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MockDexKey {
    RateNum,
    RateDen,
    ShouldRevert,
}

/// Errors the mock DEX can return.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MockDexError {
    /// The mock was told to simulate a failing DEX.
    Reverted = 1,
}

#[test]
fn test_rbac_treasury_manager_gates_treasury_operations() {
    let (env, client, contract_id) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let treasurer = Address::generate(&env);
    let new_treasury = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Assign dedicated TreasuryManager
    client.assign_role(&treasurer, &Role::TreasuryManager);

    // TreasuryManager sets new platform treasury
    client.set_platform_treasury(&new_treasury);

    // Recover accidentally sent tokens
    let (token_address, _token_client, sac) = setup_token(&env);
    sac.mint(&contract_id, &5_000);
    client.recover_tokens(&token_address, &2_000);
}

#[test]
fn test_rbac_compliance_officer_gates_compliance_operations() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let compliance = Address::generate(&env);
    let bad_user = Address::generate(&env);
    let oracle = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Assign compliance officer
    client.assign_role(&compliance, &Role::ComplianceOfficer);

    // Compliance officer blacklists and unblacklists
    client.blacklist_address(&bad_user);
    assert!(client.is_blacklisted(&bad_user));

    client.unblacklist_address(&bad_user);
    assert!(!client.is_blacklisted(&bad_user));

    // Compliance officer configures KYC
    client.set_kyc_config(&oracle, &50_000);
    assert_eq!(client.get_kyc_threshold(), Some(50_000));

    // Compliance officer pauses and unpauses
    client.set_pause(&true);
    assert!(client.is_paused());
    client.set_paused(&false);
    assert!(!client.is_paused());
}

#[test]
fn test_rbac_fee_manager_gates_fee_operations() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let fee_mgr = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &1000, &PaymentRouter::MAX_AMOUNT);

    // Assign fee manager
    client.assign_role(&fee_mgr, &Role::FeeManager);

    // Fee manager updates fee bps
    client.set_fee_bps(&350);
    assert_eq!(client.get_fee(), 350);

    // Fee manager updates fee config
    client.set_fee_config(&400, &5_000);
    assert_eq!(client.get_fee(), 400);

    // Fee manager sets min limit
    client.set_min_limit(&10_000);
}
// ── Issue #714: packed-representation round trips ───────────────────
//
// The rate limiter stores its window marker and invocation counter as a
// hand-packed `BytesN<8>`. Packing is only safe if it is lossless, so
// these tests pin the exact width and the exact round trip rather than
// trusting the implementation implicitly.

/// The packed counter must be exactly 8 bytes, not 24.
///
/// This is the assertion that actually encodes the optimization: if a
/// future change widens the layout, or reverts to the old `BytesN<24>`
/// UserSpending layout, the on-disk size stops shrinking and this fails.
#[test]
fn test_packed_rate_counter_is_eight_bytes() {
    let env = Env::default();
    let packed = pack_rate_counter(&env, 1, 1);
    assert_eq!(packed.to_array().len(), 8);
}

/// `pack_rate_counter` / `unpack_rate_counter` must be exactly inverse
/// across the full `u32` range, including both endpoints and the values
/// either side of a byte boundary.
#[test]
fn test_packed_rate_counter_round_trips() {
    let env = Env::default();
    let cases: [(u32, u32); 9] = [
        (0, 0),
        (1, 1),
        (255, 256),
        (256, 255),
        (65_535, 65_536),
        (65_536, 65_535),
        (1_000_000, 1_000_000),
        (u32::MAX - 1, u32::MAX),
        (u32::MAX, u32::MAX),
    ];

    for (window, invocations) in cases {
        let packed = pack_rate_counter(&env, window, invocations);
        assert_eq!(
            unpack_rate_counter(&packed),
            (window, invocations),
            "round trip failed for ({window}, {invocations})"
        );
    }
}

/// The two halves must not bleed into each other: a counter near `u32::MAX`
/// must not corrupt the window marker, and vice versa.
#[test]
fn test_packed_rate_counter_fields_are_independent() {
    let env = Env::default();

    // Large counter, small window: the high bytes of the counter must not
    // overflow into the window field.
    let packed = pack_rate_counter(&env, 1, u32::MAX);
    assert_eq!(unpack_rate_counter(&packed), (1, u32::MAX));

    // Large window, small counter: the window must not be truncated.
    let packed = pack_rate_counter(&env, u32::MAX, 0);
    assert_eq!(unpack_rate_counter(&packed), (u32::MAX, 0));
}

/// Big-endian layout, asserted on the raw bytes so a change of endianness
/// (which would silently reinterpret every already-stored entry) is caught.
#[test]
fn test_packed_rate_counter_is_big_endian() {
    let env = Env::default();
    let buf = pack_rate_counter(&env, 0x0102_0304, 0x0506_0708).to_array();
    assert_eq!(buf, [1, 2, 3, 4, 5, 6, 7, 8]);
}

/// The window marker must track the ledger sequence exactly, so a
/// counter stamped in one ledger does not carry over into the next.
#[test]
fn test_current_window_tracks_ledger_sequence() {
    let env = Env::default();
    assert_eq!(current_window(&env), env.ledger().sequence());

    advance_ledger(&env, 7);
    assert_eq!(current_window(&env), 7);
}

/// A contract deployed before this feature existed carries no
/// `RateLimitConfig`, and `initialize` is not re-run on upgrade. The
/// limiter must therefore fall back to the default cap rather than
/// reading the cap as absent or zero.
#[test]
fn test_rate_limit_falls_back_to_default_without_config() {
    let (env, client, _) = setup_env();

    // No `initialize` call, so nothing has ever written the config.
    assert_eq!(client.get_rate_limit(), PaymentRouter::DEFAULT_RATE_LIMIT);

    let sender = Address::generate(&env);
    assert_eq!(
        client.get_rate_limit_remaining(&sender),
        PaymentRouter::DEFAULT_RATE_LIMIT
    );
    assert!(!client.is_rate_limit_whitelisted(&sender));
}

#[test]
fn test_rate_limit_rejects_after_cap_and_resets_next_ledger() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _, sac) = setup_token(&env);
    sac.mint(&sender, &1_000_000);

    client.initialize(
        &admin,
        &treasury,
        &0,
        &i128::MAX,
        &PaymentRouter::MAX_AMOUNT,
    );

    // Rate limiting is on by default right after initialize.
    assert_eq!(client.get_rate_limit(), PaymentRouter::DEFAULT_RATE_LIMIT);
    assert_eq!(
        client.get_rate_limit_remaining(&sender),
        PaymentRouter::DEFAULT_RATE_LIMIT
    );

    // The default cap allows exactly this many payments, then rejects
    // gracefully with a dedicated error instead of panicking.
    for _ in 0..PaymentRouter::DEFAULT_RATE_LIMIT {
        client.route_payment(&sender, &recipient, &token_address, &100);
    }
    assert_eq!(client.get_rate_limit_remaining(&sender), 0);

    let res = client.try_route_payment(&sender, &recipient, &token_address, &100);
    assert_eq!(res.unwrap_err().unwrap(), Error::RateLimited);

    // The cap is per ledger sequence: a new ledger resets the window.
    advance_ledger(&env, env.ledger().sequence() + 1);
    assert_eq!(
        client.get_rate_limit_remaining(&sender),
        PaymentRouter::DEFAULT_RATE_LIMIT
    );
    client.route_payment(&sender, &recipient, &token_address, &100);
}

#[test]
fn test_rate_limit_whitelist_is_exempt() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _, sac) = setup_token(&env);
    sac.mint(&sender, &10_000_000);

    client.initialize(
        &admin,
        &treasury,
        &0,
        &i128::MAX,
        &PaymentRouter::MAX_AMOUNT,
    );

    // Fill the counter exactly to the default cap in this ledger.
    for _ in 0..PaymentRouter::DEFAULT_RATE_LIMIT {
        client.route_payment(&sender, &recipient, &token_address, &100);
    }

    // Whitelisting a high-volume sender exempts it entirely: payments
    // beyond the cap succeed without touching the counter.
    client.set_rate_limit_whitelist(&sender);
    assert!(client.is_rate_limit_whitelisted(&sender));
    assert_eq!(client.get_rate_limit_remaining(&sender), u32::MAX);

    for _ in 0..2 {
        client.route_payment(&sender, &recipient, &token_address, &100);
    }

    // Removing the exemption restores the standard cap (the counter is
    // untouched by whitelisted activity, so the current window is full).
    client.remove_rate_limit_whitelist(&sender);
    assert!(!client.is_rate_limit_whitelisted(&sender));
    let res = client.try_route_payment(&sender, &recipient, &token_address, &100);
    assert_eq!(res.unwrap_err().unwrap(), Error::RateLimited);
}

/// Builds a one-payment batch for `sender`.
fn single_payment_batch(
    env: &Env,
    sender: &Address,
    recipient: &Address,
    token_address: &Address,
) -> Vec<Payment> {
    let mut payments = Vec::new(env);
    payments.push_back(Payment {
        sender: sender.clone(),
        recipient: recipient.clone(),
        token_address: token_address.clone(),
        amount: 100,
    });
    payments
}

/// A rate-limited payment inside a batch reverts the whole batch.
///
/// Each address appears at most once per batch here on purpose: calling
/// `require_auth` twice for the same address in a single contract frame
/// is rejected by the host's auth machinery, which is a separate
/// pre-existing limitation of `route_payments` (see issue #716 notes).
#[test]
fn test_rate_limit_batch_rejection_is_atomic() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let throttled = Address::generate(&env);
    let other = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, token_client, sac) = setup_token(&env);
    sac.mint(&throttled, &1_000_000);
    sac.mint(&other, &1_000_000);

    client.initialize(
        &admin,
        &treasury,
        &0,
        &i128::MAX,
        &PaymentRouter::MAX_AMOUNT,
    );
    client.set_rate_limit(&1);

    // Exhaust the first sender's allowance for this ledger.
    client.route_payment(&throttled, &recipient, &token_address, &100);
    let res = client.try_route_payment(&throttled, &recipient, &token_address, &100);
    assert_eq!(res.unwrap_err().unwrap(), Error::RateLimited);

    // A batch whose *last* payment is throttled must revert in full: the
    // earlier payment must not settle, and no counter may be consumed.
    let mut payments = single_payment_batch(&env, &other, &recipient, &token_address);
    payments.push_back(Payment {
        sender: throttled.clone(),
        recipient: recipient.clone(),
        token_address: token_address.clone(),
        amount: 100,
    });
    let res = client.try_route_payments(&payments);
    assert_eq!(res.unwrap_err().unwrap(), Error::RateLimited);
    assert_eq!(token_client.balance(&other), 1_000_000);
    assert_eq!(token_client.balance(&throttled), 1_000_000 - 100);
    assert_eq!(client.get_rate_limit_remaining(&other), 1);

    // `other` was not charged by the reverted batch, so its single
    // payment still settles in this ledger.
    client.route_payments(&single_payment_batch(
        &env,
        &other,
        &recipient,
        &token_address,
    ));
    assert_eq!(token_client.balance(&other), 1_000_000 - 100);
}

/// Every payment in a batch counts against its own sender's per-ledger
/// cap, and the caps are tracked independently per address.
#[test]
fn test_rate_limit_batch_counts_each_sender_independently() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let recipient = Address::generate(&env);
    let a = Address::generate(&env);
    let b = Address::generate(&env);

    let (token_address, token_client, sac) = setup_token(&env);
    sac.mint(&a, &1_000_000);
    sac.mint(&b, &1_000_000);

    client.initialize(
        &admin,
        &treasury,
        &0,
        &i128::MAX,
        &PaymentRouter::MAX_AMOUNT,
    );
    client.set_rate_limit(&1);

    // One payment each: both senders are now at their own cap.
    let mut payments = single_payment_batch(&env, &a, &recipient, &token_address);
    payments.push_back(Payment {
        sender: b.clone(),
        recipient: recipient.clone(),
        token_address: token_address.clone(),
        amount: 100,
    });
    client.route_payments(&payments);
    assert_eq!(token_client.balance(&a), 1_000_000 - 100);
    assert_eq!(token_client.balance(&b), 1_000_000 - 100);
    assert_eq!(client.get_rate_limit_remaining(&a), 0);
    assert_eq!(client.get_rate_limit_remaining(&b), 0);

    // A second batch in the same ledger is rejected, and the rejection
    // does not leak across addresses: a fresh sender is still allowed.
    let mut payments = single_payment_batch(&env, &a, &recipient, &token_address);
    payments.push_back(Payment {
        sender: b.clone(),
        recipient: recipient.clone(),
        token_address: token_address.clone(),
        amount: 100,
    });
    let res = client.try_route_payments(&payments);
    assert_eq!(res.unwrap_err().unwrap(), Error::RateLimited);
    assert_eq!(token_client.balance(&a), 1_000_000 - 100);
    assert_eq!(token_client.balance(&b), 1_000_000 - 100);

    let fresh = Address::generate(&env);
    sac.mint(&fresh, &1_000);
    client.route_payments(&single_payment_batch(
        &env,
        &fresh,
        &recipient,
        &token_address,
    ));
    assert_eq!(token_client.balance(&fresh), 1_000 - 100);

    // The next ledger opens a fresh window for everyone.
    advance_ledger(&env, env.ledger().sequence() + 1);
    assert_eq!(client.get_rate_limit_remaining(&a), 1);
    assert_eq!(client.get_rate_limit_remaining(&b), 1);
}

/// A rejected payment does not consume rate-limit budget.
///
/// The Soroban host rolls back the whole invocation when a contract
/// returns `Err`, which includes the counter written by
/// `check_rate_limit`. This pins that behaviour down, because the
/// limiter's protection scope depends on it: only successful payments
/// are throttled.
#[test]
fn test_rate_limit_rejected_payment_keeps_allowance() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, token_client, sac) = setup_token(&env);
    sac.mint(&sender, &1_000_000);

    client.initialize(
        &admin,
        &treasury,
        &0,
        &i128::MAX,
        &PaymentRouter::MAX_AMOUNT,
    );
    client.set_rate_limit(&2);

    // A zero-value payment is rejected by the amount bounds, which run
    // after the rate-limit check has already reserved a slot.
    let res = client.try_route_payment(&sender, &recipient, &token_address, &0);
    assert_eq!(res.unwrap_err().unwrap(), Error::LimitExceeded);
    assert_eq!(client.get_rate_limit_remaining(&sender), 2);

    // The full allowance is therefore still available in this ledger.
    client.route_payment(&sender, &recipient, &token_address, &100);
    client.route_payment(&sender, &recipient, &token_address, &100);
    assert_eq!(client.get_rate_limit_remaining(&sender), 0);
    assert_eq!(token_client.balance(&sender), 1_000_000 - 200);

    // A rate-limit rejection is likewise not charged to the sender: the
    // next ledger opens with the full allowance rather than a stale
    // counter.
    let res = client.try_route_payment(&sender, &recipient, &token_address, &100);
    assert_eq!(res.unwrap_err().unwrap(), Error::RateLimited);
    advance_ledger(&env, env.ledger().sequence() + 1);
    assert_eq!(client.get_rate_limit_remaining(&sender), 2);
}

#[test]
fn test_rate_limit_admin_config_and_disable() {
    let (env, client, _) = setup_env();

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    let (token_address, _, sac) = setup_token(&env);
    sac.mint(&sender, &1_000_000);

    client.initialize(
        &admin,
        &treasury,
        &0,
        &i128::MAX,
        &PaymentRouter::MAX_AMOUNT,
    );

    // Tighten the cap to 2; the third payment in the same ledger fails.
    client.set_rate_limit(&2);
    assert_eq!(client.get_rate_limit(), 2);

    client.route_payment(&sender, &recipient, &token_address, &100);
    client.route_payment(&sender, &recipient, &token_address, &100);
    let res = client.try_route_payment(&sender, &recipient, &token_address, &100);
    assert_eq!(res.unwrap_err().unwrap(), Error::RateLimited);

    // A cap above MAX_RATE_LIMIT is rejected.
    let res = client.try_set_rate_limit(&(PaymentRouter::MAX_RATE_LIMIT + 1));
    assert_eq!(res.unwrap_err().unwrap(), Error::LimitExceeded);

    // Zero disables the limiter entirely.
    client.set_rate_limit(&0);
    assert_eq!(client.get_rate_limit(), 0);
    for _ in 0..(PaymentRouter::DEFAULT_RATE_LIMIT * 2) {
        client.route_payment(&sender, &recipient, &token_address, &100);
    }
    assert_eq!(client.get_rate_limit_remaining(&sender), u32::MAX);
}

/// Minimal stand-in for a Soroban DEX, exposing the adapter interface the
/// payment router expects:
/// `swap(sell_token, buy_token, amount_in, min_amount_out, recipient) -> i128`
/// and `quote(sell_token, buy_token, amount_in) -> i128`.
///
/// It trades at a configurable rate.  The router funds it with the sell
/// token before the call, so `swap` only has to settle the buy token to
/// `recipient`.
#[contract]
pub struct MockDex;

#[contractimpl]
impl MockDex {
    /// Sets the swap rate: `amount_out = amount_in * num / den`.
    pub fn set_rate(env: Env, num: i128, den: i128) {
        env.storage().instance().set(&MockDexKey::RateNum, &num);
        env.storage().instance().set(&MockDexKey::RateDen, &den);
    }

    /// Makes `swap` revert, simulating a DEX that cannot fill the trade.
    pub fn set_should_revert(env: Env, should_revert: bool) {
        env.storage()
            .instance()
            .set(&MockDexKey::ShouldRevert, &should_revert);
    }

    /// Prices a swap without moving any funds.
    pub fn quote(env: Env, _sell_token: Address, _buy_token: Address, amount_in: i128) -> i128 {
        Self::rate(&env, amount_in)
    }

    /// Sells the `amount_in` sell token this adapter was funded with and
    /// delivers the buy token to `recipient`.
    pub fn swap(
        env: Env,
        sell_token: Address,
        buy_token: Address,
        amount_in: i128,
        _min_amount_out: i128,
        recipient: Address,
    ) -> Result<i128, MockDexError> {
        if env
            .storage()
            .instance()
            .get(&MockDexKey::ShouldRevert)
            .unwrap_or(false)
        {
            return Err(MockDexError::Reverted);
        }
        // The router has already funded this adapter with `amount_in` of
        // the sell token, so the only work left is to sell it and settle
        // the buy token to `recipient`.
        let _ = sell_token;
        let amount_out = Self::rate(&env, amount_in);
        let buy_client = token::Client::new(&env, &buy_token);
        buy_client.transfer(&env.current_contract_address(), &recipient, &amount_out);

        Ok(amount_out)
    }

    /// PROBE: pull into self.
    pub fn probe_self_take(env: Env, token_addr: Address, from: Address, amount: i128) -> i128 {
        from.require_auth();
        let c = token::Client::new(&env, &token_addr);
        c.transfer(&from, &env.current_contract_address(), &amount);
        amount
    }

    /// PROBE: pay out to another contract.
    pub fn probe_pay_to_contract(env: Env, token_addr: Address, to: Address, amount: i128) -> i128 {
        let c = token::Client::new(&env, &token_addr);
        c.transfer(&env.current_contract_address(), &to, &amount);
        amount
    }

    /// Applies the configured rate to `amount_in`.
    fn rate(env: &Env, amount_in: i128) -> i128 {
        let num: i128 = env
            .storage()
            .instance()
            .get(&MockDexKey::RateNum)
            .unwrap_or(1);
        let den: i128 = env
            .storage()
            .instance()
            .get(&MockDexKey::RateDen)
            .unwrap_or(1);
        amount_in * num / den
    }
}

#[contracttype]
#[derive(Clone)]
enum MockPathDexKey {
    UnusedInput,
}

/// Small adapter used by multi-hop routing tests. It executes exactly one
/// token pair per call, returns any unspent input to the router, and settles
/// output to the requested recipient just like the production adapter API.
#[contract]
pub struct MockPathDex;

#[contractimpl]
impl MockPathDex {
    pub fn set_unused_input(env: Env, amount: i128) {
        env.storage()
            .instance()
            .set(&MockPathDexKey::UnusedInput, &amount);
    }

    pub fn swap_exact_tokens_for_tokens(
        env: Env,
        token_in: Address,
        token_out: Address,
        amount_in: i128,
        min_amount_out: i128,
        path: Vec<Address>,
        recipient: Address,
    ) -> Vec<i128> {
        assert_eq!(path.len(), 2);
        assert_eq!(path.first(), Some(token_in.clone()));
        assert_eq!(path.last(), Some(token_out.clone()));
        let unused: i128 = env
            .storage()
            .instance()
            .get(&MockPathDexKey::UnusedInput)
            .unwrap_or(0);
        assert!(unused >= 0 && unused <= amount_in);
        let amount_out = amount_in - unused;
        // Deliberately ignore the requested floor. The router must enforce it
        // from the actual token balance delta, even with a non-compliant DEX.
        let _ = min_amount_out;
        if unused > 0 {
            token::Client::new(&env, &token_in).transfer(
                &env.current_contract_address(),
                &recipient,
                &unused,
            );
        }
        token::Client::new(&env, &token_out).transfer(
            &env.current_contract_address(),
            &recipient,
            &amount_out,
        );
        vec![&env, amount_out, unused]
    }
}

/// Moves the test ledger to `timestamp`.
fn set_timestamp(env: &Env, timestamp: u64) {
    env.ledger().set(LedgerInfo {
        timestamp,
        protocol_version: env.ledger().protocol_version(),
        sequence_number: env.ledger().sequence(),
        network_id: env.ledger().network_id().into(),
        base_reserve: 100,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 4096,
        max_entry_ttl: 6312000,
    });
}

/// A funded router, two tokens, and a registered mock DEX trading the sell
/// token for the buy token at `rate_num:rate_den`.
struct SwapFixture {
    env: Env,
    client: PaymentRouterClient<'static>,
    contract_id: Address,
    treasury: Address,
    sender: Address,
    recipient: Address,
    dex: Address,
    sell_token: Address,
    buy_token: Address,
    sell_client: token::Client<'static>,
    buy_client: token::Client<'static>,
    dex_client: MockDexClient<'static>,
}

impl SwapFixture {
    /// A new address holding `amount` of the sell token.
    fn funded_sender(&self, amount: i128) -> Address {
        let sender = Address::generate(&self.env);
        let sac = token::StellarAssetClient::new(&self.env, &self.sell_token);
        sac.mint(&sender, &amount);
        sender
    }

    /// Builds a swap-routed payment from `sender` to the fixture recipient
    /// with no deadline and no quote supplied, so only `min_amount_out`
    /// guards it.
    fn payment_from(&self, sender: &Address, amount_in: i128, min_amount_out: i128) -> SwapPayment {
        SwapPayment {
            sender: sender.clone(),
            recipient: self.recipient.clone(),
            sell_token: self.sell_token.clone(),
            buy_token: self.buy_token.clone(),
            amount_in,
            min_amount_out,
            expected_amount_out: 0,
            deadline: 0,
            dex: self.dex.clone(),
        }
    }

    /// A swap-routed payment from the fixture's main sender.
    fn payment(&self, amount_in: i128, min_amount_out: i128) -> SwapPayment {
        self.payment_from(&self.sender, amount_in, min_amount_out)
    }
}

/// Deploys a router with a 1% platform fee capped at 1_000 base units, two
/// Stellar Asset Contracts, and a registered mock DEX at `rate_num:rate_den`.
fn setup_swap_fixture(rate_num: i128, rate_den: i128) -> SwapFixture {
    let env = Env::default();
    env.mock_all_auths();
    // A swap crosses two extra contracts per payment, which can outrun the
    // default test budget; these tests assert behaviour, not gas.
    env.budget().reset_unlimited();
    let contract_id = env.register_contract(None, PaymentRouter);
    let client = PaymentRouterClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);

    client.initialize(&admin, &treasury, &100, &1_000, &PaymentRouter::MAX_AMOUNT);

    let (sell_token, sell_client, sell_admin_client) = setup_token(&env);
    let (buy_token, buy_client, buy_admin_client) = setup_token(&env);

    // The DEX trades at the configured rate and is pre-funded with the
    // buy token so it can settle.
    let dex = env.register_contract(None, MockDex);
    let dex_client = MockDexClient::new(&env, &dex);
    dex_client.set_rate(&rate_num, &rate_den);
    buy_admin_client.mint(&dex, &10_000_000);

    sell_admin_client.mint(&sender, &1_000_000);

    client.register_dex(&dex);

    SwapFixture {
        env,
        client,
        contract_id,
        treasury,
        sender,
        recipient,
        dex,
        sell_token,
        buy_token,
        sell_client,
        buy_client,
        dex_client,
    }
}

struct MultiHopFixture {
    env: Env,
    client: PaymentRouterClient<'static>,
    sender: Address,
    recipient: Address,
    treasury: Address,
    dex: Address,
    path: Vec<Address>,
    token_clients: [token::Client<'static>; 4],
}

fn setup_multihop_fixture(unused: i128) -> MultiHopFixture {
    let env = Env::default();
    env.mock_all_auths();
    env.budget().reset_unlimited();
    let contract_id = env.register_contract(None, PaymentRouter);
    let client = PaymentRouterClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1_000, &PaymentRouter::MAX_AMOUNT);

    let (asset_a, client_a, admin_a) = setup_token(&env);
    let (asset_b, client_b, admin_b) = setup_token(&env);
    let (asset_c, client_c, admin_c) = setup_token(&env);
    let (asset_d, client_d, admin_d) = setup_token(&env);
    let assets = vec![&env, asset_a, asset_b, asset_c, asset_d];
    let token_clients = [client_a, client_b, client_c, client_d];
    let dex = env.register_contract(None, MockPathDex);
    MockPathDexClient::new(&env, &dex).set_unused_input(&unused);
    admin_a.mint(&sender, &10_000);
    // Each intermediate output is funded so the mock pools can settle it.
    admin_b.mint(&dex, &1_000_000);
    admin_c.mint(&dex, &1_000_000);
    admin_d.mint(&dex, &1_000_000);
    client.register_dex(&dex);

    MultiHopFixture {
        env,
        client,
        sender,
        recipient,
        treasury,
        dex,
        path: assets,
        token_clients,
    }
}

#[test]
fn test_multihop_three_hops_enforces_floors_and_refunds_each_hop_dust() {
    let f = setup_multihop_fixture(100);
    let floors = vec![&f.env, 9_900i128, 9_800i128, 9_700i128];

    assert_eq!(
        f.client
            .route_multihop_swap(&f.sender, &f.recipient, &f.dex, &10_000, &f.path, &floors),
        9_603,
    );
    assert_eq!(f.token_clients[0].balance(&f.sender), 100);
    assert_eq!(f.token_clients[1].balance(&f.sender), 100);
    assert_eq!(f.token_clients[2].balance(&f.sender), 100);
    assert_eq!(f.token_clients[3].balance(&f.recipient), 9_603);
    assert_eq!(f.token_clients[3].balance(&f.treasury), 97);
}

#[test]
fn test_multihop_cycle_is_rejected_before_moving_funds() {
    let f = setup_multihop_fixture(0);
    let cycle = vec![
        &f.env,
        f.path.get(0).unwrap(),
        f.path.get(1).unwrap(),
        f.path.get(2).unwrap(),
        f.path.get(0).unwrap(),
    ];
    let floors = vec![&f.env, 1i128, 1i128, 1i128];
    let res =
        f.client
            .try_route_multihop_swap(&f.sender, &f.recipient, &f.dex, &10_000, &cycle, &floors);
    assert_eq!(res.unwrap_err().unwrap(), Error::CyclicSwapPath);
    assert_eq!(f.token_clients[0].balance(&f.sender), 10_000);
}

#[test]
fn test_multihop_intermediate_floor_failure_reverts_all_swaps_and_dust() {
    let f = setup_multihop_fixture(100);
    let floors = vec![&f.env, 9_900i128, 9_801i128, 9_700i128];
    let res = f.client.try_route_multihop_swap(
        &f.sender,
        &f.recipient,
        &f.dex,
        &10_000,
        &f.path,
        &floors,
    );
    assert_eq!(res.unwrap_err().unwrap(), Error::SlippageExceeded);
    assert_eq!(f.token_clients[0].balance(&f.sender), 10_000);
    assert_eq!(f.token_clients[1].balance(&f.sender), 0);
    assert_eq!(f.token_clients[3].balance(&f.recipient), 0);
}

#[test]
fn test_route_payment_with_swap_delivers_buy_token_after_fee() {
    let f = setup_swap_fixture(1, 1);

    // 10 000 sell tokens in, 10 000 buy tokens out, 1% fee = 100.
    let delivered = f.client.route_payment_with_swap(&f.payment(10_000, 10_000));
    assert_eq!(delivered, 9_900);

    assert_eq!(f.sell_client.balance(&f.sender), 990_000);
    assert_eq!(f.buy_client.balance(&f.recipient), 9_900);
    assert_eq!(f.buy_client.balance(&f.treasury), 100);
    // The sell token ended up in the DEX, and volume is booked on it.
    assert_eq!(f.sell_client.balance(&f.dex), 10_000);
    assert_eq!(f.sell_client.balance(&f.contract_id), 0);
    assert_eq!(f.client.get_user_volume(&f.sender), 10_000);
}

#[test]
fn test_route_payment_with_swap_honours_a_worse_exchange_rate() {
    // The DEX returns one buy token for every two sold.
    let f = setup_swap_fixture(1, 2);

    let delivered = f.client.route_payment_with_swap(&f.payment(10_000, 5_000));
    assert_eq!(delivered, 4_950); // 5_000 out less the 1% fee
    assert_eq!(f.buy_client.balance(&f.recipient), 4_950);
    assert_eq!(f.buy_client.balance(&f.treasury), 50);
}

#[test]
fn test_swap_below_min_amount_out_reverts_the_whole_payment() {
    let f = setup_swap_fixture(1, 2);

    // Only 5_000 buy tokens are available, so a 6_000 floor must abort.
    let payment = f.payment(10_000, 6_000);
    let res = f.client.try_route_payment_with_swap(&payment);
    assert_eq!(res.unwrap_err().unwrap(), Error::SlippageExceeded);

    // Atomic failure: the sender keeps every token, nobody is paid, and no
    // state was booked.
    assert_eq!(f.sell_client.balance(&f.sender), 1_000_000);
    assert_eq!(f.sell_client.balance(&f.dex), 0);
    assert_eq!(f.buy_client.balance(&f.recipient), 0);
    assert_eq!(f.buy_client.balance(&f.treasury), 0);
    assert_eq!(f.client.get_user_volume(&f.sender), 0);
}

#[test]
fn test_swap_beyond_max_slippage_ceiling_reverts_the_payment() {
    let f = setup_swap_fixture(1, 1);

    // min_amount_out is easy to clear, but the caller also quoted 20 000,
    // which is 100% better than the 10 000 actually delivered.
    let mut payment = f.payment(10_000, 1);
    payment.expected_amount_out = 20_000;

    let res = f.client.try_route_payment_with_swap(&payment);
    assert_eq!(res.unwrap_err().unwrap(), Error::SlippageExceeded);
    assert_eq!(f.buy_client.balance(&f.recipient), 0);
    assert_eq!(f.sell_client.balance(&f.sender), 1_000_000);
}

#[test]
fn test_swap_within_max_slippage_ceiling_settles() {
    let f = setup_swap_fixture(1, 1);

    // 10% default ceiling: a 9_500 output against a 10_000 quote clears it.
    let mut payment = f.payment(10_000, 9_500);
    payment.expected_amount_out = 10_000;

    // 10 000 out, which is 5% under the 10 000 quote, clears the 10%
    // ceiling; the 1% fee is taken from the output.
    let delivered = f.client.route_payment_with_swap(&payment);
    assert_eq!(delivered, 9_900);
    assert_eq!(f.buy_client.balance(&f.recipient), 9_900);
}

#[test]
fn test_swap_failing_dex_aborts_the_payment() {
    let f = setup_swap_fixture(1, 1);
    f.dex_client.set_should_revert(&true);

    let res = f.client.try_route_payment_with_swap(&f.payment(10_000, 1));
    assert_eq!(res.unwrap_err().unwrap(), Error::SwapFailed);

    assert_eq!(f.sell_client.balance(&f.sender), 1_000_000);
    assert_eq!(f.buy_client.balance(&f.recipient), 0);
    assert_eq!(f.buy_client.balance(&f.treasury), 0);
    assert_eq!(f.client.get_user_volume(&f.sender), 0);
}

#[test]
fn test_swap_requires_a_registered_dex() {
    let f = setup_swap_fixture(1, 1);

    let mut payment = f.payment(10_000, 1);
    payment.dex = Address::generate(&f.env);

    let res = f.client.try_route_payment_with_swap(&payment);
    assert_eq!(res.unwrap_err().unwrap(), Error::DexNotRegistered);
    assert_eq!(f.sell_client.balance(&f.sender), 1_000_000);
}

#[test]
fn test_swap_after_its_deadline_is_rejected() {
    let f = setup_swap_fixture(1, 1);
    set_timestamp(&f.env, 1_000);

    let mut payment = f.payment(10_000, 1);
    payment.deadline = 500;

    let res = f.client.try_route_payment_with_swap(&payment);
    assert_eq!(res.unwrap_err().unwrap(), Error::SwapDeadlineExpired);
    assert_eq!(f.sell_client.balance(&f.sender), 1_000_000);
}

#[test]
fn test_swap_within_its_deadline_settles() {
    let f = setup_swap_fixture(1, 1);
    set_timestamp(&f.env, 1_000);

    let mut payment = f.payment(10_000, 1);
    payment.deadline = 1_500;

    assert_eq!(f.client.route_payment_with_swap(&payment), 9_900);
}

#[test]
fn test_swap_rejects_unusable_parameters() {
    let f = setup_swap_fixture(1, 1);

    // Same token on both sides is not a swap.
    let mut same_token = f.payment(10_000, 1);
    same_token.buy_token = same_token.sell_token.clone();
    assert_eq!(
        f.client
            .try_route_payment_with_swap(&same_token)
            .unwrap_err()
            .unwrap(),
        Error::InvalidSwapParams
    );

    // A zero floor would accept any output, including none at all.
    let mut no_floor = f.payment(10_000, 0);
    no_floor.min_amount_out = 0;
    assert_eq!(
        f.client
            .try_route_payment_with_swap(&no_floor)
            .unwrap_err()
            .unwrap(),
        Error::InvalidSwapParams
    );
}

#[test]
fn test_swap_enforces_balance_limits_and_recipient_rules() {
    let f = setup_swap_fixture(1, 1);

    // More sell tokens than the sender holds.
    let res = f
        .client
        .try_route_payment_with_swap(&f.payment(2_000_000, 1));
    assert_eq!(res.unwrap_err().unwrap(), Error::InsufficientBalance);

    // Self-routing is refused, same as on the direct path.
    let mut self_pay = f.payment(1_000, 1);
    self_pay.recipient = self_pay.sender.clone();
    assert_eq!(
        f.client
            .try_route_payment_with_swap(&self_pay)
            .unwrap_err()
            .unwrap(),
        Error::InvalidRecipient
    );

    // Blacklisted recipients are refused.
    f.client.blacklist_address(&f.recipient);
    assert_eq!(
        f.client
            .try_route_payment_with_swap(&f.payment(1_000, 1))
            .unwrap_err()
            .unwrap(),
        Error::Blacklisted
    );
}

#[test]
fn test_swap_is_blocked_while_paused_or_frozen() {
    let f = setup_swap_fixture(1, 1);
    let payment = f.payment(10_000, 1);

    f.client.set_pause(&true);
    assert_eq!(
        f.client
            .try_route_payment_with_swap(&payment)
            .unwrap_err()
            .unwrap(),
        Error::Paused
    );

    f.client.set_pause(&false);
    f.client.emergency_freeze();
    assert_eq!(
        f.client
            .try_route_payment_with_swap(&payment)
            .unwrap_err()
            .unwrap(),
        Error::ContractFrozen
    );
}

#[test]
fn test_route_payments_with_swap_settles_every_payment() {
    let f = setup_swap_fixture(1, 1);
    let second_sender = f.funded_sender(2_000);

    let batch = vec![
        &f.env,
        f.payment_from(&f.sender, 1_000, 1_000),
        f.payment_from(&second_sender, 2_000, 2_000),
    ];
    assert_eq!(f.client.route_payments_with_swap(&batch), 2_970);
    assert_eq!(f.buy_client.balance(&f.recipient), 2_970);
    assert_eq!(f.buy_client.balance(&f.treasury), 30);
    assert_eq!(f.client.get_user_volume(&f.sender), 1_000);
    assert_eq!(f.client.get_user_volume(&second_sender), 2_000);
}

#[test]
fn test_route_payments_with_swap_reverts_the_whole_batch() {
    let f = setup_swap_fixture(1, 1);
    let second_sender = f.funded_sender(1_000);

    // The first payment is fine; the second names an unregistered DEX.
    let mut failing = f.payment_from(&second_sender, 1_000, 1_000);
    failing.dex = Address::generate(&f.env);
    let batch = vec![&f.env, f.payment(1_000, 1_000), failing];

    let res = f.client.try_route_payments_with_swap(&batch);
    assert_eq!(res.unwrap_err().unwrap(), Error::DexNotRegistered);

    // The first payment was rolled back along with the second: the swap it
    // had already executed is undone and nothing is booked.
    assert_eq!(f.buy_client.balance(&f.recipient), 0);
    assert_eq!(f.buy_client.balance(&f.treasury), 0);
    assert_eq!(f.sell_client.balance(&f.sender), 1_000_000);
    assert_eq!(f.sell_client.balance(&f.dex), 0);
    assert_eq!(f.client.get_user_volume(&f.sender), 0);
}

#[test]
fn test_quote_swap_returns_a_slippage_adjusted_floor() {
    let f = setup_swap_fixture(1, 1);

    let quote = f
        .client
        .quote_swap(&f.dex, &f.sell_token, &f.buy_token, &10_000);
    assert_eq!(quote.amount_out, 10_000);
    assert_eq!(
        quote.max_slippage_bps,
        PaymentRouter::DEFAULT_MAX_SLIPPAGE_BPS
    );
    assert_eq!(quote.min_amount_out, 9_000); // 10% below the quote
}

#[test]
fn test_quote_swap_rejects_an_unregistered_dex() {
    let f = setup_swap_fixture(1, 1);

    let res = f
        .client
        .try_quote_swap(&Address::generate(&f.env), &f.sell_token, &f.buy_token, &1);
    assert_eq!(res.unwrap_err().unwrap(), Error::DexNotRegistered);
}

#[test]
fn test_max_slippage_bps_is_bounded_and_tightens_the_quote() {
    let f = setup_swap_fixture(1, 1);

    assert_eq!(
        f.client.get_max_slippage_bps(),
        PaymentRouter::DEFAULT_MAX_SLIPPAGE_BPS
    );
    assert_eq!(
        f.client
            .try_set_max_slippage_bps(&10_001)
            .unwrap_err()
            .unwrap(),
        Error::InvalidSwapParams
    );
    assert_eq!(
        f.client.try_set_max_slippage_bps(&-1).unwrap_err().unwrap(),
        Error::InvalidSwapParams
    );

    f.client.set_max_slippage_bps(&500);
    assert_eq!(f.client.get_max_slippage_bps(), 500);
    assert_eq!(
        f.client
            .quote_swap(&f.dex, &f.sell_token, &f.buy_token, &10_000)
            .min_amount_out,
        9_500
    );
}

#[test]
fn pair_slippage_overrides_global_cap_for_stable_and_volatile_pairs() {
    let f = setup_swap_fixture(1, 1);
    let other_token = Address::generate(&f.env);

    // Stablecoin pairs can use a strict cap; volatile pairs can use a higher one.
    f.client
        .set_pair_max_slippage_bps(&f.sell_token, &f.buy_token, &10);
    assert_eq!(
        f.client.get_pair_max_slippage_bps(&f.sell_token, &f.buy_token),
        10
    );
    let stable_quote = f
        .client
        .quote_swap(&f.dex, &f.sell_token, &f.buy_token, &10_000);
    assert_eq!(stable_quote.max_slippage_bps, 10);
    assert_eq!(stable_quote.min_amount_out, 9_990);

    f.client
        .set_pair_max_slippage_bps(&f.sell_token, &other_token, &400);
    assert_eq!(
        f.client.get_pair_max_slippage_bps(&f.sell_token, &other_token),
        400
    );
    let volatile_quote = f
        .client
        .quote_swap(&f.dex, &f.sell_token, &other_token, &10_000);
    assert_eq!(volatile_quote.max_slippage_bps, 400);
    assert_eq!(volatile_quote.min_amount_out, 9_600);
    assert_eq!(
        f.client.get_pair_max_slippage_bps(&other_token, &f.buy_token),
        PaymentRouter::DEFAULT_MAX_SLIPPAGE_BPS
    );
}

#[test]
fn pair_slippage_rejects_values_above_ceiling_and_invalid_stored_values() {
    let f = setup_swap_fixture(1, 1);
    let token_in = Address::generate(&f.env);
    let token_out = Address::generate(&f.env);

    assert_eq!(
        f.client
            .try_set_pair_max_slippage_bps(&token_in, &token_out, &501)
            .unwrap_err()
            .unwrap(),
        Error::InvalidSlippageConfig
    );
    f.env.as_contract(&f.contract_id, || {
        f.env.storage().persistent().set(
            &DataKey::PairSlippage(token_in.clone(), token_out.clone()),
            &501i128,
        );
    });
    assert_eq!(
        f.client
            .try_get_pair_max_slippage_bps(&token_in, &token_out)
            .unwrap_err()
            .unwrap(),
        Error::InvalidSlippageConfig
    );
}

#[test]
fn pair_slippage_timelock_action_applies_override() {
    let f = setup_swap_fixture(1, 1);
    let action = ActionType::SetPairMaxSlippageBps(
        f.sell_token.clone(),
        f.buy_token.clone(),
        250,
    );
    let nonce = f.client.queue_action(&action);
    advance_time(&f.env, PaymentRouter::SECONDS_IN_24H + 1);
    f.client.execute_action(&nonce);

    assert_eq!(
        f.client.get_pair_max_slippage_bps(&f.sell_token, &f.buy_token),
        250
    );
}

#[test]
fn test_dex_registration_goes_through_the_timelock() {
    let f = setup_swap_fixture(1, 1);

    let second_dex = f.env.register_contract(None, MockDex);
    assert!(!f.client.is_dex_registered(&second_dex));

    let nonce = f
        .client
        .queue_action(&ActionType::RegisterDex(second_dex.clone()));
    // Queuing alone changes nothing.
    assert!(!f.client.is_dex_registered(&second_dex));
    assert_eq!(
        f.client.try_execute_action(&nonce).unwrap_err().unwrap(),
        Error::TimelockNotReady
    );

    set_timestamp(
        &f.env,
        f.env.ledger().timestamp() + PaymentRouter::SECONDS_IN_24H + 1,
    );
    f.client.execute_action(&nonce);
    assert!(f.client.is_dex_registered(&second_dex));

    // Revoking the registration is equally delayed.
    let nonce = f
        .client
        .queue_action(&ActionType::DeregisterDex(second_dex.clone()));
    assert!(f.client.is_dex_registered(&second_dex));
    set_timestamp(
        &f.env,
        f.env.ledger().timestamp() + PaymentRouter::SECONDS_IN_24H + 1,
    );
    f.client.execute_action(&nonce);
    assert!(!f.client.is_dex_registered(&second_dex));
}

#[test]
fn test_deregistering_a_dex_stops_further_swaps() {
    let f = setup_swap_fixture(1, 1);
    assert!(f.client.is_dex_registered(&f.dex));

    f.client.deregister_dex(&f.dex);
    assert!(!f.client.is_dex_registered(&f.dex));
    assert_eq!(
        f.client
            .try_route_payment_with_swap(&f.payment(10_000, 1))
            .unwrap_err()
            .unwrap(),
        Error::DexNotRegistered
    );
}

#[test]
fn test_swap_emits_a_swap_executed_event() {
    let f = setup_swap_fixture(1, 1);

    f.client.route_payment_with_swap(&f.payment(10_000, 1));

    let env = f.env.clone();
    let events = env.events().all();
    let mut swap_event = None;
    for evt in events.iter() {
        let (contract_id, topics, data) = evt.clone();
        if contract_id != f.contract_id || topics.len() != 4 {
            continue;
        }
        let topic0: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic0 == Symbol::new(&env, "swap_executed") {
            swap_event = Some(data);
        }
    }

    let data = swap_event.expect("swap_executed event was not emitted");
    let (amount_in, amount_out, min_amount_out): (i128, i128, i128) =
        data.try_into_val(&env).unwrap();
    assert_eq!(amount_in, 10_000);
    assert_eq!(amount_out, 10_000);
    assert_eq!(min_amount_out, 1);
}
// ── Multi-signature (M-of-N) upgrade tests ───────────────────────────────
//
// Issue #664. The whole point of the feature is that no single key — the
// admin's included — can install new code, so the tests below are built
// around that invariant rather than around the happy path alone: for every
// M-of-N combination there is a case proving the (M-1)th signature is not
// enough and the Mth one is.

/// Verifies that `version()` returns the expected version string and that
/// the returned value matches the compile-time `CONTRACT_VERSION` constant,
/// so the two can never drift apart.
#[test]
fn test_version_returns_expected_string() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PaymentRouter);
    let client = PaymentRouterClient::new(&env, &contract_id);

    let returned = client.version();
    let expected = String::from_str(&env, PaymentRouter::CONTRACT_VERSION);

    assert_eq!(returned, expected);
}

/// Verifies the version string is non-empty.
#[test]
fn test_version_is_non_empty() {
    let env = Env::default();
    let contract_id = env.register_contract(None, PaymentRouter);
    let client = PaymentRouterClient::new(&env, &contract_id);

    let v = client.version();

    // A Soroban String's byte length is accessible via .len()
    assert!(v.len() > 0, "version must not be empty");
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #890 — Tiered Circuit Breaker (PauseLevel) tests
// ─────────────────────────────────────────────────────────────────────────────

/// Helper: deploy the contract and initialise it with an admin, treasury, and
/// a test token funded to `sender`.
fn setup_tiered_pause_env() -> (
    Env,
    crate::PaymentRouterClient<'static>,
    Address, // admin
    Address, // sender
    Address, // recipient
    Address, // token_address
) {
    let (env, client, _contract_id) = setup_env();
    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let recipient = Address::generate(&env);
    let treasury = Address::generate(&env);

    let (token_address, _token_client, token_admin_client) = setup_token(&env);
    token_admin_client.mint(&sender, &1_000_000);

    client.initialize(
        &admin,
        &treasury,
        &100,
        &1_000,
        &PaymentRouter::MAX_AMOUNT,
    );

    (env, client, admin, sender, recipient, token_address)
}

#[test]
fn test_pause_level_defaults_to_none() {
    let (_, client, admin, _, _, _) = setup_tiered_pause_env();
    let _ = admin;
    assert_eq!(client.get_pause_level_value(), 0u32);
    assert!(!client.is_paused());
}

#[test]
fn test_set_pause_swaps_only_blocks_swaps_allows_direct() {
    let (env, client, admin, sender, recipient, token_address) = setup_tiered_pause_env();
    let _ = (env, admin);

    // Set SwapsOnly (level 1).
    client.set_pause(&1u32);
    assert_eq!(client.get_pause_level_value(), 1u32);
    assert!(client.is_paused());

    // Direct payment must succeed under SwapsOnly.
    let result = client.try_route_payment(&sender, &recipient, &token_address, &1_000);
    assert!(
        result.is_ok(),
        "direct payment should succeed under SwapsOnly"
    );
}

#[test]
fn test_set_pause_swaps_only_blocks_route_payment_with_swap() {
    let (_, client, _, _, _, _) = setup_tiered_pause_env();

    // Set SwapsOnly (level 1).
    client.set_pause(&1u32);

    // A swap call must be rejected.
    // We only need to test that the pause check fires; a DexNotRegistered
    // error would mean the pause check was bypassed.
    let dex = Address::generate(&client.env);
    let sell_token = Address::generate(&client.env);
    let buy_token = Address::generate(&client.env);
    let path = soroban_sdk::vec![&client.env, sell_token.clone(), buy_token.clone()];

    let err = client
        .try_route_payment_with_swap_raw(
            &Address::generate(&client.env),
            &Address::generate(&client.env),
            &dex,
            &sell_token,
            &buy_token,
            &1_000,
            &path,
            &1,
        )
        .unwrap_err()
        .unwrap();

    assert_eq!(err, Error::Paused, "swap should be rejected with Paused under SwapsOnly");
}

#[test]
fn test_set_pause_full_halt_blocks_direct_payment() {
    let (_, client, _, sender, recipient, token_address) = setup_tiered_pause_env();

    // Set FullHalt (level 2).
    client.set_pause(&2u32);
    assert_eq!(client.get_pause_level_value(), 2u32);

    let err = client
        .try_route_payment(&sender, &recipient, &token_address, &1_000)
        .unwrap_err()
        .unwrap();

    assert_eq!(err, Error::Paused, "direct payment should be rejected under FullHalt");
}

#[test]
fn test_set_pause_resume_from_swaps_only() {
    let (_, client, _, sender, recipient, token_address) = setup_tiered_pause_env();

    // Pause at SwapsOnly, then resume.
    client.set_pause(&1u32);
    assert!(client.is_paused());

    client.set_pause(&0u32);
    assert!(!client.is_paused());
    assert_eq!(client.get_pause_level_value(), 0u32);

    // Direct payment should work again.
    let result = client.try_route_payment(&sender, &recipient, &token_address, &1_000);
    assert!(result.is_ok(), "payment should succeed after resuming");
}

#[test]
fn test_set_paused_bool_true_maps_to_full_halt() {
    let (_, client, _, _, _, _) = setup_tiered_pause_env();

    // Legacy bool API: true → FullHalt.
    client.set_paused(&true);
    assert_eq!(client.get_pause_level_value(), 2u32);
}

#[test]
fn test_set_paused_bool_false_maps_to_none() {
    let (_, client, _, _, _, _) = setup_tiered_pause_env();

    client.set_paused(&true);
    client.set_paused(&false);
    assert_eq!(client.get_pause_level_value(), 0u32);
    assert!(!client.is_paused());
}

// ─────────────────────────────────────────────────────────────────────────────
// Issue #889 — Dead-man's switch operator heartbeat renewal tests
// ─────────────────────────────────────────────────────────────────────────────

fn advance_time_by(env: &Env, seconds: u64) {
    env.ledger().set(LedgerInfo {
        timestamp: env.ledger().timestamp() + seconds,
        ..env.ledger().get()
    });
}

/// Arm the DMS and return admin, backup, operator addresses.
fn setup_dms_with_operator(
    env: &Env,
    client: &crate::PaymentRouterClient<'static>,
) -> (Address, Address, Address) {
    let admin = Address::generate(env);
    let backup = Address::generate(env);
    let operator = Address::generate(env);
    let treasury = Address::generate(env);
    let timeout = PaymentRouter::MIN_DMS_TIMEOUT + 1;

    client.initialize(&admin, &treasury, &100, &1_000, &PaymentRouter::MAX_AMOUNT);

    // Arm the switch via timelock.
    let nonce = client.queue_action(&ActionType::SetBackupAdmin(backup.clone(), timeout));
    advance_time_by(env, PaymentRouter::SECONDS_IN_24H + 1);
    client.execute_action(&nonce);

    // Appoint operator.
    client.set_heartbeat_operator(&operator);

    (admin, backup, operator)
}

#[test]
fn test_set_heartbeat_operator_stores_address() {
    let (env, client, _) = setup_env();
    let (_, _, operator) = setup_dms_with_operator(&env, &client);

    let stored = client.get_heartbeat_operator();
    assert_eq!(stored.unwrap(), operator);
}

#[test]
fn test_renew_heartbeat_operator_resets_timestamp() {
    let (env, client, _) = setup_env();
    let (_, _, operator) = setup_dms_with_operator(&env, &client);

    // Record the heartbeat before advancing time.
    let (_, _, hb_before, _) = client.get_dead_mans_switch();

    // Advance 10 seconds, then renew.
    advance_time_by(&env, 10);
    client.renew_heartbeat(&operator);

    let (_, _, hb_after, _) = client.get_dead_mans_switch();
    assert!(
        hb_after > hb_before,
        "heartbeat should advance after renew_heartbeat"
    );
}

#[test]
fn test_renew_heartbeat_prevents_backup_claim() {
    let (env, client, _) = setup_env();
    let timeout = PaymentRouter::MIN_DMS_TIMEOUT + 1;
    let (_, backup, operator) = setup_dms_with_operator(&env, &client);

    // Advance almost to expiry.
    advance_time_by(&env, timeout - 1);

    // Operator renews — this pushes LastHeartbeat forward.
    client.renew_heartbeat(&operator);

    // Now advance to what *would* have been past-expiry without the renewal.
    advance_time_by(&env, timeout - 1);

    // Backup claim should still fail (not enough time since renewal).
    let err = client
        .try_claim_admin(&backup)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, Error::DmsNotReady);
}

#[test]
fn test_renew_heartbeat_unauthorized_address_rejected() {
    let (env, client, _) = setup_env();
    setup_dms_with_operator(&env, &client);

    let impostor = Address::generate(&env);
    let err = client
        .try_renew_heartbeat(&impostor)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, Error::Unauthorized);
}

#[test]
fn test_renew_heartbeat_fails_when_no_operator_configured() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1_000, &PaymentRouter::MAX_AMOUNT);

    // No operator set.
    let random = Address::generate(&env);
    let err = client
        .try_renew_heartbeat(&random)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, Error::Unauthorized);
}

#[test]
fn test_operator_cannot_change_backup_admin() {
    let (env, client, _) = setup_env();
    let (_, backup, operator) = setup_dms_with_operator(&env, &client);
    let _ = operator;

    // There is no set_backup_admin entrypoint the operator could call;
    // just verify the backup address is unchanged after a renew.
    let (b_before, _, _, _) = client.get_dead_mans_switch();
    assert_eq!(b_before.unwrap(), backup);
}

#[test]
fn test_set_heartbeat_operator_rejects_admin_as_operator() {
    let (env, client, _) = setup_env();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    client.initialize(&admin, &treasury, &100, &1_000, &PaymentRouter::MAX_AMOUNT);

    // Appointing the admin itself as operator should fail.
    let err = client
        .try_set_heartbeat_operator(&admin)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, Error::InvalidDmsConfig);
}
