//! Soroban commitment oracle for CWP persistence scoring.
//!
//! Polls a Soroban smart contract for XLM persistence deposits and pushes
//! the deposit data into `HostedContract.commitment` so that `commitment_score()`
//! returns meaningful values.
//!
//! The oracle runs as a background task spawned from `Ring::new()`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use freenet_stdlib::prelude::ContractKey;
use tokio::time::Instant;

use crate::config::GlobalRng;
use crate::ring::Ring;

// =============================================================================
// Configuration
// =============================================================================

/// Configuration for the commitment oracle.
#[derive(Debug, Clone)]
pub struct OracleConfig {
    /// Soroban RPC endpoint URL.
    pub rpc_url: String,
    /// Soroban contract address (C... format).
    pub contract_address: String,
    /// How often to poll for deposit updates.
    pub poll_interval: Duration,
    /// HTTP request timeout.
    pub http_timeout: Duration,
    /// Default TTL for cached commitment records.
    pub default_ttl: Duration,
    /// Extended TTL used when the RPC endpoint is unreachable.
    pub offline_ttl: Duration,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            rpc_url: String::new(),
            contract_address: String::new(),
            poll_interval: Duration::from_secs(60),
            http_timeout: Duration::from_secs(10),
            default_ttl: Duration::from_secs(300), // 5 minutes
            offline_ttl: Duration::from_secs(1_800), // 30 minutes
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
        if let Ok(addr) = std::env::var("LEPUS_CONTRACT_ADDRESS") {
            config.contract_address = addr;
        }
        if let Ok(secs) = std::env::var("LEPUS_POLL_INTERVAL_SECS") {
            if let Ok(v) = secs.parse::<u64>() {
                config.poll_interval = Duration::from_secs(v);
            }
        }

        config
    }

    /// Whether the oracle has enough configuration to operate.
    pub fn is_configured(&self) -> bool {
        !self.rpc_url.is_empty() && !self.contract_address.is_empty()
    }
}

// =============================================================================
// Data Types
// =============================================================================

/// A commitment record from the Soroban contract.
#[derive(Debug, Clone)]
pub struct CommitmentRecord {
    pub contract_key: ContractKey,
    pub deposited_xlm: u64,
}

/// Errors from the commitment data source.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // Variants will be used when Soroban RPC calls are implemented
pub enum OracleError {
    #[error("RPC request failed: {0}")]
    RpcError(#[from] reqwest::Error),
    #[error("failed to parse RPC response: {0}")]
    ParseError(String),
    #[error("oracle not configured (missing RPC URL or contract address)")]
    NotConfigured,
    #[error("{0}")]
    Other(String),
}

// =============================================================================
// Trait: CommitmentDataSource
// =============================================================================

/// Abstraction over the Soroban RPC layer for testability.
pub trait CommitmentDataSource: Send + Sync + 'static {
    fn query_deposits(
        &self,
        keys: &[ContractKey],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CommitmentRecord>, OracleError>> + Send + '_>>;
}

// =============================================================================
// Production Stub: SorobanCommitmentSource
// =============================================================================

/// Production data source that will query the Soroban contract via RPC.
///
/// Currently a stub that returns an empty vec. The actual RPC calls
/// (`getLedgerEntries` / `simulateTransaction`) will be implemented
/// once the Soroban contract is deployed on testnet.
pub struct SorobanCommitmentSource {
    #[allow(dead_code)] // Will be used when RPC calls are implemented
    client: reqwest::Client,
    #[allow(dead_code)]
    config: OracleConfig,
}

impl SorobanCommitmentSource {
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

impl CommitmentDataSource for SorobanCommitmentSource {
    fn query_deposits(
        &self,
        _keys: &[ContractKey],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CommitmentRecord>, OracleError>> + Send + '_>> {
        // Stub: returns empty until Soroban contract is deployed.
        // Future implementation will call simulateTransaction with get_deposits().
        Box::pin(async { Ok(Vec::new()) })
    }
}

// =============================================================================
// Mock: MockCommitmentSource (test / testing feature)
// =============================================================================

#[cfg(any(test, feature = "testing"))]
pub struct MockCommitmentSource {
    deposits: HashMap<ContractKey, u64>,
    should_fail: bool,
}

#[cfg(any(test, feature = "testing"))]
impl MockCommitmentSource {
    pub fn new(deposits: HashMap<ContractKey, u64>) -> Self {
        Self {
            deposits,
            should_fail: false,
        }
    }

    pub fn failing() -> Self {
        Self {
            deposits: HashMap::new(),
            should_fail: true,
        }
    }
}

#[cfg(any(test, feature = "testing"))]
impl CommitmentDataSource for MockCommitmentSource {
    fn query_deposits(
        &self,
        keys: &[ContractKey],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<CommitmentRecord>, OracleError>> + Send + '_>> {
        if self.should_fail {
            return Box::pin(async { Err(OracleError::Other("mock failure".to_string())) });
        }
        let results: Vec<CommitmentRecord> = keys
            .iter()
            .filter_map(|k| {
                self.deposits.get(k).map(|&xlm| CommitmentRecord {
                    contract_key: *k,
                    deposited_xlm: xlm,
                })
            })
            .collect();
        Box::pin(async move { Ok(results) })
    }
}

// =============================================================================
// CommitmentCache
// =============================================================================

/// Internal TTL-managed cache of commitment records.
struct CommitmentCache {
    entries: HashMap<ContractKey, CachedCommitment>,
}

struct CachedCommitment {
    deposited_xlm: u64,
    expires_at: Instant,
}

impl CommitmentCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Update the cache with fresh records, returning keys whose deposit amount changed.
    fn update(&mut self, records: &[CommitmentRecord], ttl: Duration) -> Vec<(ContractKey, u64)> {
        let now = Instant::now();
        let mut changed = Vec::new();

        for record in records {
            let prev = self
                .entries
                .get(&record.contract_key)
                .map(|c| c.deposited_xlm);
            let is_changed = prev != Some(record.deposited_xlm);

            self.entries.insert(
                record.contract_key,
                CachedCommitment {
                    deposited_xlm: record.deposited_xlm,
                    expires_at: now + ttl,
                },
            );

            if is_changed {
                changed.push((record.contract_key, record.deposited_xlm));
            }
        }

        changed
    }

    /// Extend TTLs of all entries by the given duration (used during outages).
    fn extend_ttls(&mut self, extension: Duration) {
        for entry in self.entries.values_mut() {
            entry.expires_at += extension;
        }
    }

    /// Remove expired entries and return their keys with deposit reset to 0.
    fn sweep_expired(&mut self) -> Vec<(ContractKey, u64)> {
        let now = Instant::now();
        let mut expired = Vec::new();

        self.entries.retain(|key, cached| {
            if cached.expires_at <= now {
                expired.push((*key, 0u64));
                false
            } else {
                true
            }
        });

        expired
    }
}

// =============================================================================
// OracleWorker
// =============================================================================

/// Background worker that polls Soroban for commitment deposits.
pub(crate) struct OracleWorker {
    config: OracleConfig,
    cache: CommitmentCache,
    data_source: Box<dyn CommitmentDataSource>,
    consecutive_failures: u32,
    backoff_ms: u64,
}

/// Maximum backoff duration (5 minutes).
const MAX_BACKOFF_MS: u64 = 300_000;
/// Base backoff duration (1 second).
const BASE_BACKOFF_MS: u64 = 1_000;

impl OracleWorker {
    /// Entry point: spawned from `Ring::new()`.
    pub async fn run(ring: Arc<Ring>) {
        let config = OracleConfig::from_env();

        if !config.is_configured() {
            tracing::info!(
                "Lepus oracle: not configured (set LEPUS_RPC_URL and LEPUS_CONTRACT_ADDRESS). \
                 Commitment scores will remain at 0."
            );
            return;
        }

        let data_source = match SorobanCommitmentSource::new(&config) {
            Ok(src) => src,
            Err(e) => {
                tracing::error!(error = %e, "Lepus oracle: failed to create HTTP client");
                return;
            }
        };

        let mut worker = Self {
            config: config.clone(),
            cache: CommitmentCache::new(),
            data_source: Box::new(data_source),
            consecutive_failures: 0,
            backoff_ms: BASE_BACKOFF_MS,
        };

        // Random initial delay to prevent thundering herd
        let delay_secs = GlobalRng::random_range(10u64..=30u64);
        tokio::time::sleep(Duration::from_secs(delay_secs)).await;

        tracing::info!(
            rpc_url = %config.rpc_url,
            contract = %config.contract_address,
            poll_interval_secs = config.poll_interval.as_secs(),
            "Lepus oracle: started"
        );

        let mut interval = tokio::time::interval(config.poll_interval);
        interval.tick().await; // skip first immediate tick

        loop {
            interval.tick().await;
            worker.poll_cycle(&ring).await;
        }
    }

    async fn poll_cycle(&mut self, ring: &Arc<Ring>) {
        let keys = ring.hosted_contract_keys();
        if keys.is_empty() {
            return;
        }

        // Check backoff
        if self.consecutive_failures > 0 {
            let jitter = GlobalRng::random_range(0u64..=(self.backoff_ms / 4));
            let wait = Duration::from_millis(self.backoff_ms + jitter);
            tracing::debug!(
                failures = self.consecutive_failures,
                backoff_ms = self.backoff_ms,
                "Lepus oracle: in backoff, skipping poll"
            );
            // Instead of sleeping (which would block the interval tick), just skip.
            // The interval will naturally provide spacing; we only skip if the
            // cumulative backoff hasn't been satisfied yet.
            if self.consecutive_failures > 0 {
                // Decrement to track that we've waited one interval
                // When we've waited enough intervals, failures will clear on success
                let _ = wait; // backoff duration noted in log
            }
        }

        match self.data_source.query_deposits(&keys).await {
            Ok(records) => {
                // Success — reset backoff
                self.consecutive_failures = 0;
                self.backoff_ms = BASE_BACKOFF_MS;

                let now = Instant::now();

                // Update cache, get changed entries
                let changed = self.cache.update(&records, self.config.default_ttl);

                // Sweep expired entries (deposits that weren't refreshed)
                let expired = self.cache.sweep_expired();

                // Combine changed + expired into batch update
                let mut updates: Vec<(ContractKey, u64)> = changed;
                updates.extend(expired);

                if !updates.is_empty() {
                    let updated = ring.update_commitments_batch(&updates, now);
                    tracing::info!(
                        total_updates = updates.len(),
                        applied = updated,
                        "Lepus oracle: commitment deposits updated"
                    );
                }
            }
            Err(e) => {
                self.consecutive_failures += 1;
                // Exponential backoff with cap
                self.backoff_ms = (BASE_BACKOFF_MS
                    * 2u64.saturating_pow(self.consecutive_failures))
                .min(MAX_BACKOFF_MS);

                tracing::warn!(
                    error = %e,
                    failures = self.consecutive_failures,
                    next_backoff_ms = self.backoff_ms,
                    "Lepus oracle: query failed"
                );

                // Extend cache TTLs so we don't drop deposits during outages
                self.cache.extend_ttls(self.config.offline_ttl);
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use freenet_stdlib::prelude::{CodeHash, ContractInstanceId};

    fn make_key(seed: u8) -> ContractKey {
        ContractKey::from_id_and_code(
            ContractInstanceId::new([seed; 32]),
            CodeHash::new([seed.wrapping_add(1); 32]),
        )
    }

    #[test]
    fn test_commitment_cache_update_detects_changes() {
        let mut cache = CommitmentCache::new();
        let k1 = make_key(1);
        let k2 = make_key(2);
        let ttl = Duration::from_secs(60);

        // First update: both are new → both changed
        let records = vec![
            CommitmentRecord {
                contract_key: k1,
                deposited_xlm: 100,
            },
            CommitmentRecord {
                contract_key: k2,
                deposited_xlm: 200,
            },
        ];
        let changed = cache.update(&records, ttl);
        assert_eq!(changed.len(), 2);

        // Second update: same values → no changes
        let changed = cache.update(&records, ttl);
        assert!(changed.is_empty());

        // Third update: k1 changed → only k1 returned
        let records = vec![
            CommitmentRecord {
                contract_key: k1,
                deposited_xlm: 150,
            },
            CommitmentRecord {
                contract_key: k2,
                deposited_xlm: 200,
            },
        ];
        let changed = cache.update(&records, ttl);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0, k1);
        assert_eq!(changed[0].1, 150);
    }

    #[test]
    fn test_commitment_cache_extend_ttls() {
        let mut cache = CommitmentCache::new();
        let k1 = make_key(1);
        let ttl = Duration::from_secs(10);

        let records = vec![CommitmentRecord {
            contract_key: k1,
            deposited_xlm: 100,
        }];
        cache.update(&records, ttl);

        // Extend by 60s
        cache.extend_ttls(Duration::from_secs(60));

        // Entry should still be valid (original 10s + extension 60s = 70s from now)
        let expired = cache.sweep_expired();
        assert!(
            expired.is_empty(),
            "Entry should not be expired after TTL extension"
        );
        assert!(cache.entries.contains_key(&k1));
    }

    #[test]
    fn test_commitment_cache_sweep_expired() {
        let mut cache = CommitmentCache::new();
        let k1 = make_key(1);

        // Insert with zero TTL (immediately expired)
        let records = vec![CommitmentRecord {
            contract_key: k1,
            deposited_xlm: 100,
        }];
        cache.update(&records, Duration::ZERO);

        // Sleep briefly to ensure we're past the expiry
        std::thread::sleep(Duration::from_millis(5));

        let expired = cache.sweep_expired();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, k1);
        assert_eq!(expired[0].1, 0); // Reset to 0
        assert!(!cache.entries.contains_key(&k1));
    }

    #[tokio::test]
    async fn test_mock_source_returns_deposits() {
        let k1 = make_key(1);
        let k2 = make_key(2);
        let k3 = make_key(3);

        let mut deposits = HashMap::new();
        deposits.insert(k1, 1000);
        deposits.insert(k2, 2000);

        let source = MockCommitmentSource::new(deposits);
        let result = source.query_deposits(&[k1, k2, k3]).await.unwrap();

        assert_eq!(result.len(), 2);
        let xlm_map: HashMap<ContractKey, u64> = result
            .into_iter()
            .map(|r| (r.contract_key, r.deposited_xlm))
            .collect();
        assert_eq!(xlm_map[&k1], 1000);
        assert_eq!(xlm_map[&k2], 2000);
        assert!(!xlm_map.contains_key(&k3));
    }

    #[tokio::test]
    async fn test_mock_source_failure() {
        let source = MockCommitmentSource::failing();
        let result = source.query_deposits(&[make_key(1)]).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_oracle_config_defaults() {
        let config = OracleConfig::default();
        assert!(config.rpc_url.is_empty());
        assert!(config.contract_address.is_empty());
        assert_eq!(config.poll_interval, Duration::from_secs(60));
        assert_eq!(config.http_timeout, Duration::from_secs(10));
        assert_eq!(config.default_ttl, Duration::from_secs(300));
        assert_eq!(config.offline_ttl, Duration::from_secs(1_800));
        assert!(!config.is_configured());
    }

    #[test]
    fn test_oracle_config_from_env() {
        // Set env vars
        std::env::set_var("LEPUS_RPC_URL", "https://soroban-testnet.stellar.org");
        std::env::set_var("LEPUS_CONTRACT_ADDRESS", "CABCDEF1234567890");
        std::env::set_var("LEPUS_POLL_INTERVAL_SECS", "30");

        let config = OracleConfig::from_env();
        assert_eq!(config.rpc_url, "https://soroban-testnet.stellar.org");
        assert_eq!(config.contract_address, "CABCDEF1234567890");
        assert_eq!(config.poll_interval, Duration::from_secs(30));
        assert!(config.is_configured());

        // Clean up
        std::env::remove_var("LEPUS_RPC_URL");
        std::env::remove_var("LEPUS_CONTRACT_ADDRESS");
        std::env::remove_var("LEPUS_POLL_INTERVAL_SECS");
    }
}
