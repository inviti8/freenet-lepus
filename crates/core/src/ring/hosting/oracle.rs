//! Lepus oracle: deposit-index subscriber and Stellar proof relayer.
//!
//! ## Architecture
//!
//! All lepus-configured nodes subscribe to the deposit-index Freenet contract
//! to receive deposit updates.  A subset of nodes ("relayers") also have
//! Stellar RPC access and periodically fetch SCP proofs, then submit them as
//! UPDATEs to the deposit-index contract so the network stays up to date.
//!
//! The oracle runs as a background task spawned from `Ring::new()`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use freenet_stdlib::prelude::*;

use super::deposit_index::{self, DepositProof};
use crate::config::{GlobalExecutor, GlobalRng};
use crate::ring::Ring;

// =============================================================================
// Configuration
// =============================================================================

/// Configuration for the lepus oracle.
#[derive(Debug, Clone)]
pub struct OracleConfig {
    /// Stellar RPC endpoint URL (relayer nodes only).
    pub rpc_url: String,
    /// Hex 32-byte deposit-index ContractInstanceId.
    pub deposit_index_key: Option<String>,
    /// How often to poll for new Stellar ledgers (relayer mode).
    pub poll_interval: Duration,
    /// HTTP request timeout.
    pub http_timeout: Duration,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            rpc_url: String::new(),
            deposit_index_key: None,
            poll_interval: Duration::from_secs(60),
            http_timeout: Duration::from_secs(10),
        }
    }
}

impl OracleConfig {
    /// Build config from environment variables, falling back to defaults.
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(url) = std::env::var("LEPUS_RPC_URL") {
            config.rpc_url = url;
        }
        if let Ok(key) = std::env::var("LEPUS_DEPOSIT_INDEX_KEY") {
            if !key.trim().is_empty() {
                config.deposit_index_key = Some(key.trim().to_string());
            }
        }
        if let Ok(secs) = std::env::var("LEPUS_POLL_INTERVAL_SECS") {
            if let Ok(v) = secs.parse::<u64>() {
                config.poll_interval = Duration::from_secs(v);
            }
        }

        config
    }

    /// Whether this node should subscribe to the deposit-index contract.
    pub fn is_subscriber_configured(&self) -> bool {
        self.deposit_index_key.is_some()
    }

    /// Whether this node can relay Stellar proofs (subscriber + RPC access).
    pub fn is_relayer_configured(&self) -> bool {
        self.deposit_index_key.is_some() && !self.rpc_url.is_empty()
    }
}

// =============================================================================
// Errors
// =============================================================================

/// Errors from the oracle / Stellar proof source.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // Variants used by MockStellarProofSource (test cfg) and future RPC impl
pub enum OracleError {
    #[error("RPC request failed: {0}")]
    RpcError(#[from] reqwest::Error),
    #[error("failed to parse RPC response: {0}")]
    ParseError(String),
    #[error("oracle not configured")]
    NotConfigured,
    #[error("{0}")]
    Other(String),
}

// =============================================================================
// Trait: StellarProofSource
// =============================================================================

/// Abstraction over Stellar RPC for testability.
///
/// Relayer nodes use this to discover new ledgers with DEPOSIT events and
/// fetch the SCP proofs needed to submit UPDATE deltas to the deposit-index
/// Freenet contract.
pub trait StellarProofSource: Send + Sync + 'static {
    /// Return ledger sequence numbers (since `since_ledger`) that contain
    /// DEPOSIT events from the hvym-freenet-service Soroban contract.
    fn query_deposit_events(
        &self,
        since_ledger: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u32>, OracleError>> + Send + '_>>;

    /// Fetch the full SCP proof bundle for a given ledger.
    fn fetch_proof_for_ledger(
        &self,
        ledger_seq: u32,
    ) -> Pin<Box<dyn Future<Output = Result<DepositProof, OracleError>> + Send + '_>>;
}

// =============================================================================
// Production Stub: StellarProofRelayer
// =============================================================================

/// Production data source that will query Stellar Horizon / RPC for proofs.
///
/// Currently a stub that returns empty results. The actual RPC calls will be
/// implemented once the hvym-freenet-service contract is deployed on testnet.
pub struct StellarProofRelayer {
    #[allow(dead_code)]
    client: reqwest::Client,
    #[allow(dead_code)]
    config: OracleConfig,
}

impl StellarProofRelayer {
    pub fn new(config: &OracleConfig) -> Result<Self, OracleError> {
        let client = reqwest::Client::builder()
            .timeout(config.http_timeout)
            .build()
            .map_err(OracleError::RpcError)?;
        Ok(Self {
            client,
            config: config.clone(),
        })
    }
}

impl StellarProofSource for StellarProofRelayer {
    fn query_deposit_events(
        &self,
        _since_ledger: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u32>, OracleError>> + Send + '_>> {
        // Stub: returns empty until Soroban contract is deployed.
        Box::pin(async { Ok(Vec::new()) })
    }

    fn fetch_proof_for_ledger(
        &self,
        _ledger_seq: u32,
    ) -> Pin<Box<dyn Future<Output = Result<DepositProof, OracleError>> + Send + '_>> {
        Box::pin(async { Err(OracleError::NotConfigured) })
    }
}

// =============================================================================
// Mock: MockStellarProofSource (test / testing feature)
// =============================================================================

#[cfg(any(test, feature = "testing"))]
pub struct MockStellarProofSource {
    /// Pre-built proofs indexed by ledger sequence.
    proofs: std::collections::HashMap<u32, DepositProof>,
    /// Whether queries should fail.
    pub should_fail: bool,
}

#[cfg(any(test, feature = "testing"))]
impl MockStellarProofSource {
    pub fn new(proofs: std::collections::HashMap<u32, DepositProof>) -> Self {
        Self {
            proofs,
            should_fail: false,
        }
    }

    pub fn failing() -> Self {
        Self {
            proofs: std::collections::HashMap::new(),
            should_fail: true,
        }
    }
}

#[cfg(any(test, feature = "testing"))]
impl StellarProofSource for MockStellarProofSource {
    fn query_deposit_events(
        &self,
        since_ledger: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u32>, OracleError>> + Send + '_>> {
        if self.should_fail {
            return Box::pin(async { Err(OracleError::Other("mock failure".to_string())) });
        }
        let mut seqs: Vec<u32> = self
            .proofs
            .keys()
            .filter(|&&s| s > since_ledger)
            .copied()
            .collect();
        seqs.sort();
        Box::pin(async move { Ok(seqs) })
    }

    fn fetch_proof_for_ledger(
        &self,
        ledger_seq: u32,
    ) -> Pin<Box<dyn Future<Output = Result<DepositProof, OracleError>> + Send + '_>> {
        if self.should_fail {
            return Box::pin(async { Err(OracleError::Other("mock failure".to_string())) });
        }
        let proof = self.proofs.get(&ledger_seq).cloned();
        Box::pin(async move {
            proof.ok_or_else(|| {
                OracleError::ParseError(format!("no proof for ledger {ledger_seq}"))
            })
        })
    }
}

// =============================================================================
// Subscriber: subscribe to the deposit-index Freenet contract
// =============================================================================

/// Maximum attempts to wait for OpManager before giving up.
const OP_MANAGER_MAX_RETRIES: u32 = 60;

/// Subscribe this node to the deposit-index Freenet contract so that
/// deposit updates flow in via the normal subscription mechanism.
async fn subscribe_to_deposit_index(ring: Arc<Ring>) {
    let Some(instance_id) = deposit_index::deposit_index_instance_id() else {
        return;
    };

    // Wait for OpManager to become available
    let op_manager = {
        let mut attempt = 0u32;
        loop {
            if let Some(om) = ring.upgrade_op_manager() {
                break om;
            }
            attempt += 1;
            if attempt > OP_MANAGER_MAX_RETRIES {
                tracing::error!(
                    "Lepus subscriber: OpManager not available after {OP_MANAGER_MAX_RETRIES} retries, giving up"
                );
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };

    // Retry subscription with exponential backoff
    let mut backoff_ms: u64 = 1_000;
    const MAX_BACKOFF_MS: u64 = 60_000;

    loop {
        let sub_op = crate::operations::subscribe::start_op(instance_id, false);
        match crate::operations::subscribe::request_subscribe(&op_manager, sub_op).await {
            Ok(()) => {
                tracing::info!(
                    instance_id = %instance_id,
                    "Lepus subscriber: subscribed to deposit-index contract"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    backoff_ms,
                    "Lepus subscriber: failed to subscribe to deposit-index, retrying"
                );
                let jitter = GlobalRng::random_range(0u64..=(backoff_ms / 4));
                tokio::time::sleep(Duration::from_millis(backoff_ms + jitter)).await;
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
        }
    }
}

// =============================================================================
// Relayer: fetch SCP proofs and submit UPDATEs
// =============================================================================

/// Base backoff duration (1 second).
const BASE_BACKOFF_MS: u64 = 1_000;
/// Maximum backoff duration (5 minutes).
const RELAY_MAX_BACKOFF_MS: u64 = 300_000;

/// Relay deposit proofs from Stellar to the deposit-index Freenet contract.
///
/// Polls the Stellar RPC for new ledgers with DEPOSIT events, fetches the
/// SCP proof for each, and submits an UPDATE delta to the deposit-index
/// contract on the Freenet network.
async fn relay_deposit_proofs(
    ring: Arc<Ring>,
    source: Box<dyn StellarProofSource>,
    config: OracleConfig,
) {
    let Some(contract_key) = deposit_index::deposit_index_contract_key() else {
        tracing::error!(
            "Lepus relayer: LEPUS_DEPOSIT_INDEX_CODE_HASH not set, cannot submit UPDATEs"
        );
        return;
    };

    // Wait for OpManager
    let op_manager = {
        let mut attempt = 0u32;
        loop {
            if let Some(om) = ring.upgrade_op_manager() {
                break om;
            }
            attempt += 1;
            if attempt > OP_MANAGER_MAX_RETRIES {
                tracing::error!(
                    "Lepus relayer: OpManager not available after {OP_MANAGER_MAX_RETRIES} retries, giving up"
                );
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };

    tracing::info!(
        rpc_url = %config.rpc_url,
        poll_interval_secs = config.poll_interval.as_secs(),
        "Lepus relayer: started"
    );

    let mut last_processed_ledger: u32 = 0;
    let mut consecutive_failures: u32 = 0;
    let mut backoff_ms: u64 = BASE_BACKOFF_MS;

    let mut interval = tokio::time::interval(config.poll_interval);
    interval.tick().await; // skip first immediate tick

    loop {
        interval.tick().await;

        // Backoff on consecutive failures
        if consecutive_failures > 0 {
            let jitter = GlobalRng::random_range(0u64..=(backoff_ms / 4));
            tokio::time::sleep(Duration::from_millis(backoff_ms + jitter)).await;
        }

        // Query for new ledgers with DEPOSIT events
        let ledger_seqs = match source.query_deposit_events(last_processed_ledger).await {
            Ok(seqs) => {
                consecutive_failures = 0;
                backoff_ms = BASE_BACKOFF_MS;
                seqs
            }
            Err(e) => {
                consecutive_failures += 1;
                backoff_ms = (BASE_BACKOFF_MS
                    * 2u64.saturating_pow(consecutive_failures))
                .min(RELAY_MAX_BACKOFF_MS);
                tracing::warn!(
                    error = %e,
                    failures = consecutive_failures,
                    next_backoff_ms = backoff_ms,
                    "Lepus relayer: query_deposit_events failed"
                );
                continue;
            }
        };

        if ledger_seqs.is_empty() {
            continue;
        }

        for ledger_seq in ledger_seqs {
            // Fetch proof for this ledger
            let proof = match source.fetch_proof_for_ledger(ledger_seq).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        ledger_seq,
                        error = %e,
                        "Lepus relayer: failed to fetch proof, skipping ledger"
                    );
                    continue;
                }
            };

            // Serialize proof as JSON delta
            let json_bytes = match serde_json::to_vec(&proof) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(
                        ledger_seq,
                        error = %e,
                        "Lepus relayer: failed to serialize proof"
                    );
                    continue;
                }
            };

            let update_data =
                UpdateData::Delta(StateDelta::from(json_bytes));
            let update_op = crate::operations::update::start_op(
                contract_key,
                update_data,
                RelatedContracts::default(),
            );

            match crate::operations::update::request_update(&op_manager, update_op).await {
                Ok(()) => {
                    tracing::info!(
                        ledger_seq,
                        "Lepus relayer: submitted proof for ledger"
                    );
                    last_processed_ledger = ledger_seq;
                }
                Err(e) => {
                    tracing::warn!(
                        ledger_seq,
                        error = %e,
                        "Lepus relayer: failed to submit UPDATE"
                    );
                    // Don't advance last_processed_ledger — will retry next cycle
                    break;
                }
            }
        }
    }
}

// =============================================================================
// OracleWorker
// =============================================================================

/// Background worker that manages the deposit-index subscriber and relayer.
pub(crate) struct OracleWorker;

impl OracleWorker {
    /// Entry point: spawned from `Ring::new()`.
    pub async fn run(ring: Arc<Ring>) {
        let config = OracleConfig::from_env();

        if !config.is_subscriber_configured() {
            tracing::info!(
                "Lepus: not configured (set LEPUS_DEPOSIT_INDEX_KEY). \
                 Commitment scores will remain at 0."
            );
            return;
        }

        // All lepus nodes: subscribe to deposit-index contract
        let ring2 = ring.clone();
        GlobalExecutor::spawn(async move {
            subscribe_to_deposit_index(ring2).await;
        });

        if config.is_relayer_configured() {
            // Relayer nodes: also relay proofs from Stellar
            let source = match StellarProofRelayer::new(&config) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "Lepus relayer: failed to create HTTP client");
                    return;
                }
            };

            // Random initial delay to prevent thundering herd
            let delay_secs = GlobalRng::random_range(10u64..=30u64);
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;

            relay_deposit_proofs(ring, Box::new(source), config).await;
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oracle_config_defaults() {
        let config = OracleConfig::default();
        assert!(config.rpc_url.is_empty());
        assert!(config.deposit_index_key.is_none());
        assert_eq!(config.poll_interval, Duration::from_secs(60));
        assert_eq!(config.http_timeout, Duration::from_secs(10));
        assert!(!config.is_subscriber_configured());
        assert!(!config.is_relayer_configured());
    }

    #[test]
    fn test_oracle_config_subscriber_only() {
        std::env::set_var(
            "LEPUS_DEPOSIT_INDEX_KEY",
            "0102030405060708091011121314151617181920212223242526272829303132",
        );
        // Remove RPC URL to ensure relayer is not configured
        std::env::remove_var("LEPUS_RPC_URL");

        let config = OracleConfig::from_env();
        assert!(config.is_subscriber_configured());
        assert!(!config.is_relayer_configured());

        std::env::remove_var("LEPUS_DEPOSIT_INDEX_KEY");
    }

    #[test]
    fn test_oracle_config_relayer() {
        std::env::set_var(
            "LEPUS_DEPOSIT_INDEX_KEY",
            "0102030405060708091011121314151617181920212223242526272829303132",
        );
        std::env::set_var("LEPUS_RPC_URL", "https://horizon-testnet.stellar.org");
        std::env::set_var("LEPUS_POLL_INTERVAL_SECS", "30");

        let config = OracleConfig::from_env();
        assert!(config.is_subscriber_configured());
        assert!(config.is_relayer_configured());
        assert_eq!(config.rpc_url, "https://horizon-testnet.stellar.org");
        assert_eq!(config.poll_interval, Duration::from_secs(30));

        std::env::remove_var("LEPUS_DEPOSIT_INDEX_KEY");
        std::env::remove_var("LEPUS_RPC_URL");
        std::env::remove_var("LEPUS_POLL_INTERVAL_SECS");
    }

    #[test]
    fn test_oracle_config_empty_key_not_configured() {
        std::env::set_var("LEPUS_DEPOSIT_INDEX_KEY", "  ");
        let config = OracleConfig::from_env();
        assert!(!config.is_subscriber_configured());
        std::env::remove_var("LEPUS_DEPOSIT_INDEX_KEY");
    }

    #[tokio::test]
    async fn test_mock_source_returns_proofs() {
        let mut proofs = std::collections::HashMap::new();
        proofs.insert(
            100,
            DepositProof {
                ledger_seq: 100,
                scp_envelopes: vec!["env1".to_string()],
                transaction_set: "txset".to_string(),
                tx_result_metas: vec!["meta1".to_string()],
            },
        );
        proofs.insert(
            200,
            DepositProof {
                ledger_seq: 200,
                scp_envelopes: vec!["env2".to_string()],
                transaction_set: "txset2".to_string(),
                tx_result_metas: vec!["meta2".to_string()],
            },
        );

        let source = MockStellarProofSource::new(proofs);

        // Query all events since ledger 0
        let seqs = source.query_deposit_events(0).await.unwrap();
        assert_eq!(seqs, vec![100, 200]);

        // Query events since ledger 100 (should only return 200)
        let seqs = source.query_deposit_events(100).await.unwrap();
        assert_eq!(seqs, vec![200]);

        // Fetch specific proof
        let proof = source.fetch_proof_for_ledger(100).await.unwrap();
        assert_eq!(proof.ledger_seq, 100);

        // Fetch non-existent proof
        let result = source.fetch_proof_for_ledger(999).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_source_failure() {
        let source = MockStellarProofSource::failing();
        let result = source.query_deposit_events(0).await;
        assert!(result.is_err());

        let result = source.fetch_proof_for_ledger(1).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_stellar_proof_relayer_creation() {
        let config = OracleConfig {
            rpc_url: "https://example.com".to_string(),
            deposit_index_key: Some("abc".to_string()),
            poll_interval: Duration::from_secs(60),
            http_timeout: Duration::from_secs(10),
        };
        let relayer = StellarProofRelayer::new(&config);
        assert!(relayer.is_ok());
    }
}
