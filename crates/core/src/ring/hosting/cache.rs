//! Unified hosting cache for contract state caching.
//!
//! This module implements a byte-budget aware LRU cache with TTL protection for hosted contracts.
//! It unifies the previously separate `SeedingCache` and `GetSubscriptionCache` into a single
//! source of truth for which contracts a peer is hosting.
//!
//! # Design Principles
//!
//! 1. **Single source of truth**: All hosted contracts are tracked in one cache
//! 2. **Resource-aware eviction**: Byte-budget LRU with TTL protection
//! 3. **Subscription renewal**: All hosted contracts get subscription renewal
//! 4. **Access type tracking**: Records how contract was accessed (GET/PUT/SUBSCRIBE)

use freenet_stdlib::prelude::ContractKey;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tokio::time::Instant;

use crate::util::time_source::TimeSource;

#[cfg(feature = "lepus")]
use ordered_float::OrderedFloat;

/// Default hosting cache budget: 100MB
pub const DEFAULT_HOSTING_BUDGET_BYTES: u64 = 100 * 1024 * 1024;

/// Multiplier for TTL relative to subscription renewal interval.
/// Gives this many renewal attempts before eviction if renewals keep failing.
pub const TTL_RENEWAL_MULTIPLIER: u32 = 4;

/// Default minimum TTL before a hosted contract can be evicted.
/// Computed as TTL_RENEWAL_MULTIPLIER × SUBSCRIPTION_RENEWAL_INTERVAL.
pub const DEFAULT_MIN_TTL: Duration = Duration::from_secs(
    super::SUBSCRIPTION_RENEWAL_INTERVAL.as_secs() * TTL_RENEWAL_MULTIPLIER as u64,
);

// =============================================================================
// CWP (Commitment-Weighted Persistence) — Lepus Feature
// =============================================================================

/// Scoring weights and normalization targets for CWP eviction.
///
/// CWP replaces LRU eviction with a weighted persistence score:
///   score = w_c * commitment + w_i * identity + w_n * contribution + w_r * recency
///
/// Higher scores survive eviction longer.
#[cfg(feature = "lepus")]
#[derive(Debug, Clone)]
pub struct CWPConfig {
    /// Weight for commitment (Soroban deposit) factor.
    pub commitment_weight: f64,
    /// Weight for identity verification factor.
    pub identity_weight: f64,
    /// Weight for network contribution (bytes served / consumed) factor.
    pub contribution_weight: f64,
    /// Weight for recency of last access.
    pub recency_weight: f64,
    /// Target XLM density: deposited_xlm / size_bytes at which commitment saturates.
    pub commitment_density_target: f64,
    /// Target contribution ratio at which contribution score saturates.
    pub contribution_target: f64,
    /// Half-life in seconds for recency decay. Score = 0.5 after this many seconds.
    pub recency_halflife_secs: f64,
}

#[cfg(feature = "lepus")]
impl Default for CWPConfig {
    fn default() -> Self {
        Self {
            commitment_weight: 0.50,
            identity_weight: 0.25,
            contribution_weight: 0.15,
            recency_weight: 0.10,
            commitment_density_target: 0.001,
            contribution_target: 1.5,
            recency_halflife_secs: 604_800.0, // 7 days
        }
    }
}

/// Placeholder for Soroban commitment state (Phase 2).
#[cfg(feature = "lepus")]
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // Phase 2 placeholder — fields populated by Oracle
pub struct CommitmentState {
    /// Deposited XLM (in stroops or smallest unit).
    pub deposited_xlm: u64,
    /// Last time the Oracle verified this deposit.
    pub last_oracle_check: Option<Instant>,
}

/// Placeholder for identity verification state (Phase 3).
#[cfg(feature = "lepus")]
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // Phase 3 placeholder — fields populated by identity verifier
pub struct IdentityState {
    /// Creator's Ed25519 public key, if known.
    pub creator_pubkey: Option<[u8; 32]>,
    /// Whether the creator identity has been verified.
    pub creator_verified: bool,
    /// Subscriber's Ed25519 public key, if known.
    pub subscriber_pubkey: Option<[u8; 32]>,
    /// Whether the subscriber identity has been verified.
    pub subscriber_verified: bool,
    /// The intended recipient from the identity envelope (datapod's recipient_public_key).
    /// Used by subscription handshake to verify the remote subscriber matches.
    pub recipient_pubkey: Option<[u8; 32]>,
}

/// Type of access that adds/refreshes a contract in the hosting cache.
///
/// Only certain operations should refresh the LRU position to prevent manipulation:
/// - GET: User requesting the contract
/// - PUT: User writing new state
/// - SUBSCRIBE: User subscribing to updates
///
/// UPDATE is explicitly excluded because contract creators control when updates happen,
/// which could be abused to keep contracts cached indefinitely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessType {
    Get,
    Put,
    /// Used in tests and reserved for future use when explicit SUBSCRIBE triggers hosting
    #[cfg_attr(not(test), allow(dead_code))]
    Subscribe,
}

/// Result of recording a contract access in the hosting cache.
#[derive(Debug)]
pub struct RecordAccessResult {
    /// Whether this contract was newly added (vs. refreshed existing)
    pub is_new: bool,
    /// Contracts that were evicted to make room
    pub evicted: Vec<ContractKey>,
}

/// Metadata about a hosted contract.
#[derive(Debug, Clone)]
pub struct HostedContract {
    /// Size of the contract state in bytes
    pub size_bytes: u64,
    /// Last time this contract was accessed (via GET/PUT/SUBSCRIBE)
    pub last_accessed: Instant,
    /// Type of the last access
    pub access_type: AccessType,
    /// Soroban commitment state (Phase 2 placeholder).
    #[cfg(feature = "lepus")]
    pub commitment: CommitmentState,
    /// Identity verification state (Phase 3 placeholder).
    #[cfg(feature = "lepus")]
    pub identity: IdentityState,
    /// Total bytes served to other peers for this contract.
    #[cfg(feature = "lepus")]
    pub bytes_served: u64,
    /// Total bytes consumed (received) from other peers for this contract.
    #[cfg(feature = "lepus")]
    pub bytes_consumed: u64,
}

#[cfg(feature = "lepus")]
impl HostedContract {
    /// Compute the CWP persistence score for this contract.
    ///
    /// Higher scores indicate higher priority to keep in cache.
    /// Score is in [0.0, 1.0] — a weighted sum of four sub-scores.
    pub fn persistence_score(&self, now: Instant, config: &CWPConfig) -> f64 {
        let c = self.commitment_score(config);
        let i = self.identity_score();
        let n = self.contribution_score(config);
        let r = self.recency_score(now, config);

        let score = config.commitment_weight * c
            + config.identity_weight * i
            + config.contribution_weight * n
            + config.recency_weight * r;

        score.clamp(0.0, 1.0)
    }

    /// Commitment sub-score: `min(1.0, deposited_xlm / (size_bytes * density_target))`.
    ///
    /// Returns 0.0 when no deposit exists (Phase 1 default).
    pub fn commitment_score(&self, config: &CWPConfig) -> f64 {
        let denominator = self.size_bytes as f64 * config.commitment_density_target;
        if denominator <= 0.0 {
            return 0.0;
        }
        (self.commitment.deposited_xlm as f64 / denominator).min(1.0)
    }

    /// Identity sub-score: `(creator_verified * 0.6) + (subscriber_verified * 0.4)`.
    ///
    /// Returns 0.0 when no identity is verified (Phase 1 default).
    pub fn identity_score(&self) -> f64 {
        let creator = if self.identity.creator_verified {
            0.6
        } else {
            0.0
        };
        let subscriber = if self.identity.subscriber_verified {
            0.4
        } else {
            0.0
        };
        creator + subscriber
    }

    /// Contribution sub-score: `min(1.0, (bytes_served / max(bytes_consumed, 1)) / target)`.
    ///
    /// Rewards contracts that serve more data than they consume.
    pub fn contribution_score(&self, config: &CWPConfig) -> f64 {
        let consumed = self.bytes_consumed.max(1) as f64;
        let ratio = self.bytes_served as f64 / consumed;
        (ratio / config.contribution_target).min(1.0)
    }

    /// Recency sub-score: `1.0 / (1.0 + elapsed_secs / halflife_secs)`.
    ///
    /// Exponential-ish decay: returns 1.0 for just-accessed, 0.5 at halflife.
    pub fn recency_score(&self, now: Instant, config: &CWPConfig) -> f64 {
        let elapsed = now
            .saturating_duration_since(self.last_accessed)
            .as_secs_f64();
        1.0 / (1.0 + elapsed / config.recency_halflife_secs)
    }
}

/// Unified hosting cache that combines byte-budget LRU with TTL protection.
///
/// This cache maintains contracts that this peer is "hosting" - keeping available
/// for the network. The cache has:
/// - Byte budget: Large contracts consume more budget
/// - TTL protection: Contracts can't be evicted until min_ttl has passed
/// - LRU ordering: Oldest contracts evicted first when over budget
///
/// # Subscription Renewal
///
/// ALL contracts in this cache should have their subscriptions renewed automatically.
/// This is the key fix for the bug where GET-triggered subscriptions weren't being renewed.
pub struct HostingCache<T: TimeSource> {
    /// Maximum bytes to use for cached contracts
    budget_bytes: u64,
    /// Current total bytes used
    current_bytes: u64,
    /// Minimum time since last access before eviction is allowed
    min_ttl: Duration,
    /// LRU order - front is oldest, back is newest
    lru_order: VecDeque<ContractKey>,
    /// Contract metadata indexed by key
    contracts: HashMap<ContractKey, HostedContract>,
    /// Time source for testability
    time_source: T,
    /// CWP scoring configuration (Lepus only).
    #[cfg(feature = "lepus")]
    cwp_config: CWPConfig,
}

impl<T: TimeSource> HostingCache<T> {
    /// Create a new hosting cache with the given byte budget and TTL.
    pub fn new(budget_bytes: u64, min_ttl: Duration, time_source: T) -> Self {
        Self {
            budget_bytes,
            current_bytes: 0,
            min_ttl,
            lru_order: VecDeque::new(),
            contracts: HashMap::new(),
            time_source,
            #[cfg(feature = "lepus")]
            cwp_config: CWPConfig::default(),
        }
    }

    /// Create a new hosting cache with explicit CWP configuration.
    #[cfg(feature = "lepus")]
    #[allow(dead_code)] // Public API for custom CWP config
    pub fn new_with_cwp(
        budget_bytes: u64,
        min_ttl: Duration,
        time_source: T,
        cwp_config: CWPConfig,
    ) -> Self {
        Self {
            budget_bytes,
            current_bytes: 0,
            min_ttl,
            lru_order: VecDeque::new(),
            contracts: HashMap::new(),
            time_source,
            cwp_config,
        }
    }

    /// Record an access to a contract, adding or refreshing it in the cache.
    ///
    /// If the contract is already cached, this refreshes its LRU position and timestamp.
    /// If not cached, this adds it and evicts old contracts if necessary.
    ///
    /// Returns a `RecordAccessResult` containing:
    /// - `is_new`: Whether this contract was newly added (vs. refreshed existing)
    /// - `evicted`: Contracts that were evicted to make room (if any)
    ///
    /// Eviction respects TTL: contracts won't be evicted until min_ttl has passed.
    pub fn record_access(
        &mut self,
        key: ContractKey,
        size_bytes: u64,
        access_type: AccessType,
    ) -> RecordAccessResult {
        let now = self.time_source.now();
        let mut evicted = Vec::new();

        if let Some(existing) = self.contracts.get_mut(&key) {
            // Already cached - update size if changed and refresh position
            if existing.size_bytes != size_bytes {
                // Adjust byte accounting: add new size, subtract old size
                self.current_bytes = self
                    .current_bytes
                    .saturating_add(size_bytes)
                    .saturating_sub(existing.size_bytes);
                existing.size_bytes = size_bytes;
            }
            existing.last_accessed = now;
            existing.access_type = access_type;

            // Move to back of LRU (most recently used)
            self.lru_order.retain(|k| k != &key);
            self.lru_order.push_back(key);

            RecordAccessResult {
                is_new: false,
                evicted,
            }
        } else {
            // Not cached - need to add it
            // First, evict until we have room (respecting TTL)
            #[cfg(not(feature = "lepus"))]
            {
                while self.current_bytes + size_bytes > self.budget_bytes
                    && !self.lru_order.is_empty()
                {
                    if let Some(oldest_key) = self.lru_order.front().cloned() {
                        if let Some(oldest) = self.contracts.get(&oldest_key) {
                            let age = now.saturating_duration_since(oldest.last_accessed);
                            if age >= self.min_ttl {
                                if let Some(removed) = self.contracts.remove(&oldest_key) {
                                    self.current_bytes =
                                        self.current_bytes.saturating_sub(removed.size_bytes);
                                    self.lru_order.pop_front();
                                    evicted.push(oldest_key);
                                }
                            } else {
                                break;
                            }
                        } else {
                            self.lru_order.pop_front();
                        }
                    } else {
                        break;
                    }
                }
            }

            // CWP eviction: evict the contract with the lowest persistence score
            // among those past min_ttl. O(n) scan — acceptable for ~50K contracts.
            #[cfg(feature = "lepus")]
            {
                while self.current_bytes + size_bytes > self.budget_bytes
                    && !self.contracts.is_empty()
                {
                    let victim = self.find_lowest_score_victim(now);
                    if let Some(victim_key) = victim {
                        if let Some(removed) = self.contracts.remove(&victim_key) {
                            self.current_bytes =
                                self.current_bytes.saturating_sub(removed.size_bytes);
                            self.lru_order.retain(|k| k != &victim_key);
                            evicted.push(victim_key);
                        }
                    } else {
                        // All remaining contracts are within TTL — allow exceeding budget
                        break;
                    }
                }
            }

            // Add the new contract
            let contract = HostedContract {
                size_bytes,
                last_accessed: now,
                access_type,
                #[cfg(feature = "lepus")]
                commitment: CommitmentState::default(),
                #[cfg(feature = "lepus")]
                identity: IdentityState::default(),
                #[cfg(feature = "lepus")]
                bytes_served: 0,
                #[cfg(feature = "lepus")]
                bytes_consumed: 0,
            };
            self.contracts.insert(key, contract);
            self.lru_order.push_back(key);
            self.current_bytes = self.current_bytes.saturating_add(size_bytes);

            RecordAccessResult {
                is_new: true,
                evicted,
            }
        }
    }

    /// Touch/refresh a contract's timestamp without adding it if missing.
    ///
    /// Called when UPDATE is received for a hosted contract.
    /// This refreshes the TTL and LRU position, indicating the contract
    /// is actively receiving updates.
    pub fn touch(&mut self, key: &ContractKey) {
        if let Some(existing) = self.contracts.get_mut(key) {
            existing.last_accessed = self.time_source.now();
            // Move to back of LRU
            self.lru_order.retain(|k| k != key);
            self.lru_order.push_back(*key);
        }
    }

    /// Check if a contract is in the cache.
    pub fn contains(&self, key: &ContractKey) -> bool {
        self.contracts.contains_key(key)
    }

    /// Get metadata about a hosted contract.
    #[allow(dead_code)] // Public API for introspection
    pub fn get(&self, key: &ContractKey) -> Option<&HostedContract> {
        self.contracts.get(key)
    }

    /// Get the current number of hosted contracts.
    pub fn len(&self) -> usize {
        self.contracts.len()
    }

    /// Check if the cache is empty.
    #[allow(dead_code)] // Public API for introspection
    pub fn is_empty(&self) -> bool {
        self.contracts.is_empty()
    }

    /// Get the current bytes used.
    #[allow(dead_code)] // Public API for introspection
    pub fn current_bytes(&self) -> u64 {
        self.current_bytes
    }

    /// Get the budget in bytes.
    #[allow(dead_code)] // Public API for introspection
    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// Get all hosted contract keys in LRU order (oldest first).
    #[cfg(test)]
    pub fn keys_lru_order(&self) -> Vec<ContractKey> {
        self.lru_order.iter().cloned().collect()
    }

    /// Iterate over all hosted contract keys.
    pub fn iter(&self) -> impl Iterator<Item = ContractKey> + '_ {
        self.contracts.keys().cloned()
    }

    /// Sweep for contracts that are over budget and past TTL.
    ///
    /// The `should_retain` predicate is called for each candidate contract before eviction.
    /// If it returns `true`, the contract is skipped (kept in cache) even if over TTL.
    /// This allows protecting contracts with client subscriptions from eviction.
    ///
    /// Returns contracts evicted from this cache.
    pub fn sweep_expired<F>(&mut self, should_retain: F) -> Vec<ContractKey>
    where
        F: Fn(&ContractKey) -> bool,
    {
        let now = self.time_source.now();
        let mut evicted = Vec::new();

        #[cfg(not(feature = "lepus"))]
        {
            let mut skipped_keys = Vec::new();

            while self.current_bytes > self.budget_bytes && !self.lru_order.is_empty() {
                if let Some(oldest_key) = self.lru_order.front().cloned() {
                    if let Some(oldest) = self.contracts.get(&oldest_key) {
                        let age = now.saturating_duration_since(oldest.last_accessed);
                        if age >= self.min_ttl {
                            if should_retain(&oldest_key) {
                                self.lru_order.pop_front();
                                skipped_keys.push(oldest_key);
                                continue;
                            }
                            if let Some(removed) = self.contracts.remove(&oldest_key) {
                                self.current_bytes =
                                    self.current_bytes.saturating_sub(removed.size_bytes);
                                self.lru_order.pop_front();
                                evicted.push(oldest_key);
                            }
                        } else {
                            break;
                        }
                    } else {
                        self.lru_order.pop_front();
                    }
                } else {
                    break;
                }
            }

            for key in skipped_keys {
                self.lru_order.push_back(key);
            }
        }

        // CWP sweep: find lowest-scoring contract past min_ttl, respecting should_retain
        #[cfg(feature = "lepus")]
        {
            while self.current_bytes > self.budget_bytes && !self.contracts.is_empty() {
                let victim = self.find_lowest_score_victim_with_retain(now, &should_retain);
                if let Some(victim_key) = victim {
                    if let Some(removed) = self.contracts.remove(&victim_key) {
                        self.current_bytes = self.current_bytes.saturating_sub(removed.size_bytes);
                        self.lru_order.retain(|k| k != &victim_key);
                        evicted.push(victim_key);
                    }
                } else {
                    // All remaining are within TTL or retained
                    break;
                }
            }
        }

        evicted
    }

    /// Find the contract with the lowest CWP persistence score that is eligible
    /// for eviction (past min_ttl).
    ///
    /// Tie-breaking: lowest score → oldest last_accessed → smallest key bytes.
    #[cfg(feature = "lepus")]
    fn find_lowest_score_victim(&self, now: Instant) -> Option<ContractKey> {
        self.find_lowest_score_victim_with_retain(now, &|_| false)
    }

    /// Find the contract with the lowest CWP persistence score that is eligible
    /// for eviction (past min_ttl), respecting a should_retain predicate.
    #[cfg(feature = "lepus")]
    fn find_lowest_score_victim_with_retain(
        &self,
        now: Instant,
        should_retain: &dyn Fn(&ContractKey) -> bool,
    ) -> Option<ContractKey> {
        let mut best: Option<(OrderedFloat<f64>, Instant, ContractKey)> = None;

        for (key, contract) in &self.contracts {
            let age = now.saturating_duration_since(contract.last_accessed);
            if age < self.min_ttl {
                continue; // Protected by TTL
            }
            if should_retain(key) {
                continue; // Caller wants to keep this one
            }

            let score = OrderedFloat(contract.persistence_score(now, &self.cwp_config));
            let candidate = (score, contract.last_accessed, *key);

            let dominated = match &best {
                None => true,
                Some(current_best) => {
                    // Lower score is worse (evict first). On tie: older is worse.
                    // On tie again: compare key bytes for determinism.
                    candidate.0 < current_best.0
                        || (candidate.0 == current_best.0
                            && (candidate.1 < current_best.1
                                || (candidate.1 == current_best.1
                                    && candidate.2.id().as_bytes()
                                        < current_best.2.id().as_bytes())))
                }
            };

            if dominated {
                best = Some(candidate);
            }
        }

        best.map(|(_, _, key)| key)
    }

    /// Load a contract entry from persisted data during startup.
    ///
    /// Unlike `record_access`, this uses a pre-computed last_accessed time
    /// and doesn't evict other contracts (we may be over budget after loading).
    ///
    /// # Arguments
    /// * `key` - The contract key
    /// * `size_bytes` - Size of the contract state
    /// * `access_type` - How the contract was last accessed (GET/PUT/SUBSCRIBE)
    /// * `last_access_age` - How long ago the contract was last accessed
    pub fn load_persisted_entry(
        &mut self,
        key: ContractKey,
        size_bytes: u64,
        access_type: AccessType,
        last_access_age: Duration,
    ) {
        // Skip if already loaded (shouldn't happen, but defensive)
        if self.contracts.contains_key(&key) {
            return;
        }

        // Calculate the last_accessed time from age
        let now = self.time_source.now();
        let last_accessed = now.checked_sub(last_access_age).unwrap_or(now);

        let contract = HostedContract {
            size_bytes,
            last_accessed,
            access_type,
            #[cfg(feature = "lepus")]
            commitment: CommitmentState::default(),
            #[cfg(feature = "lepus")]
            identity: IdentityState::default(),
            #[cfg(feature = "lepus")]
            bytes_served: 0,
            #[cfg(feature = "lepus")]
            bytes_consumed: 0,
        };

        self.contracts.insert(key, contract);
        self.current_bytes = self.current_bytes.saturating_add(size_bytes);
        // Note: LRU order will be sorted after all entries are loaded
    }

    /// Sort the LRU order by last_accessed time after bulk loading.
    ///
    /// Call this after `load_persisted_entry` calls are complete.
    pub fn finalize_loading(&mut self) {
        // Build LRU order from contracts sorted by last_accessed (oldest first)
        let mut entries: Vec<_> = self
            .contracts
            .iter()
            .map(|(k, v)| (*k, v.last_accessed))
            .collect();
        entries.sort_by_key(|(_, last_accessed)| *last_accessed);

        self.lru_order.clear();
        for (key, _) in entries {
            self.lru_order.push_back(key);
        }
    }

    /// Record bytes served (sent to other peers) for a hosted contract.
    #[cfg(feature = "lepus")]
    pub fn record_bytes_served(&mut self, key: &ContractKey, bytes: u64) {
        if let Some(contract) = self.contracts.get_mut(key) {
            contract.bytes_served = contract.bytes_served.saturating_add(bytes);
        }
    }

    /// Record bytes consumed (received from other peers) for a hosted contract.
    #[cfg(feature = "lepus")]
    pub fn record_bytes_consumed(&mut self, key: &ContractKey, bytes: u64) {
        if let Some(contract) = self.contracts.get_mut(key) {
            contract.bytes_consumed = contract.bytes_consumed.saturating_add(bytes);
        }
    }

    /// Get a mutable reference to a hosted contract's metadata.
    #[cfg(feature = "lepus")]
    #[allow(dead_code)] // Public API for future Oracle/identity integration
    pub fn get_mut(&mut self, key: &ContractKey) -> Option<&mut HostedContract> {
        self.contracts.get_mut(key)
    }

    /// Get all hosted contract keys.
    #[cfg(feature = "lepus")]
    pub fn contract_keys(&self) -> Vec<ContractKey> {
        self.contracts.keys().cloned().collect()
    }

    /// Update the identity verification state for a hosted contract.
    ///
    /// Sets creator and subscriber identity fields on the contract's
    /// `IdentityState`. Returns `true` if the key was found.
    #[cfg(feature = "lepus")]
    pub fn update_identity(
        &mut self,
        key: &ContractKey,
        creator_pubkey: Option<[u8; 32]>,
        creator_verified: bool,
        subscriber_pubkey: Option<[u8; 32]>,
        subscriber_verified: bool,
        recipient_pubkey: Option<[u8; 32]>,
    ) -> bool {
        if let Some(contract) = self.contracts.get_mut(key) {
            contract.identity.creator_pubkey = creator_pubkey;
            contract.identity.creator_verified = creator_verified;
            contract.identity.subscriber_pubkey = subscriber_pubkey;
            contract.identity.subscriber_verified = subscriber_verified;
            contract.identity.recipient_pubkey = recipient_pubkey;
            true
        } else {
            false
        }
    }

    /// Update subscriber identity from the subscription handshake.
    ///
    /// Verifies whether the declared subscriber pubkey matches the datapod's
    /// `recipient_pubkey` from the identity envelope, or if the content is public.
    /// Returns `true` if the key was found.
    #[cfg(feature = "lepus")]
    pub fn update_subscriber_identity(
        &mut self,
        key: &ContractKey,
        subscriber_pubkey: &[u8; 32],
    ) -> bool {
        if let Some(contract) = self.contracts.get_mut(key) {
            contract.identity.subscriber_pubkey = Some(*subscriber_pubkey);
            contract.identity.subscriber_verified = match &contract.identity.recipient_pubkey {
                Some(recipient) => {
                    // Public content ([0u8;32]) verifies any subscriber
                    *recipient == [0u8; 32] || recipient == subscriber_pubkey
                }
                None => false, // No envelope parsed yet
            };
            true
        } else {
            false
        }
    }

    /// Count active subscriptions for a given identity pubkey.
    #[cfg(feature = "lepus")]
    pub fn count_subscriptions_for_identity(&self, pubkey: &[u8; 32]) -> usize {
        self.contracts
            .values()
            .filter(|c| c.identity.subscriber_pubkey.as_ref() == Some(pubkey))
            .count()
    }

    /// Check if a subscriber identity has any funded contract (deposited_xlm > 0).
    #[cfg(feature = "lepus")]
    pub fn is_identity_funded(&self, pubkey: &[u8; 32]) -> bool {
        self.contracts.values().any(|c| {
            c.identity.subscriber_pubkey.as_ref() == Some(pubkey) && c.commitment.deposited_xlm > 0
        })
    }

    /// Update the commitment deposit for a hosted contract.
    ///
    /// Sets `deposited_xlm` and `last_oracle_check` on the contract's
    /// `CommitmentState`. Returns `true` if the key was found.
    #[cfg(feature = "lepus")]
    pub fn update_commitment(
        &mut self,
        key: &ContractKey,
        deposited_xlm: u64,
        check_time: Instant,
    ) -> bool {
        if let Some(contract) = self.contracts.get_mut(key) {
            contract.commitment.deposited_xlm = deposited_xlm;
            contract.commitment.last_oracle_check = Some(check_time);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::time_source::SharedMockTimeSource;
    use freenet_stdlib::prelude::{CodeHash, ContractInstanceId};

    fn make_key(seed: u8) -> ContractKey {
        ContractKey::from_id_and_code(
            ContractInstanceId::new([seed; 32]),
            CodeHash::new([seed.wrapping_add(1); 32]),
        )
    }

    fn make_cache(
        budget: u64,
        min_ttl: Duration,
    ) -> (HostingCache<SharedMockTimeSource>, SharedMockTimeSource) {
        let time_source = SharedMockTimeSource::new();
        let cache = HostingCache::new(budget, min_ttl, time_source.clone());
        (cache, time_source)
    }

    #[test]
    fn test_empty_cache() {
        let (cache, _) = make_cache(1000, Duration::from_secs(60));
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.current_bytes(), 0);
        assert!(!cache.contains(&make_key(1)));
    }

    #[test]
    fn test_add_single_contract() {
        let (mut cache, _) = make_cache(1000, Duration::from_secs(60));
        let key = make_key(1);

        let result = cache.record_access(key, 100, AccessType::Get);

        assert!(result.is_new);
        assert!(result.evicted.is_empty());
        assert!(cache.contains(&key));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.current_bytes(), 100);

        let info = cache.get(&key).unwrap();
        assert_eq!(info.size_bytes, 100);
        assert_eq!(info.access_type, AccessType::Get);
    }

    #[test]
    fn test_refresh_existing_contract() {
        let (mut cache, time) = make_cache(1000, Duration::from_secs(60));
        let key = make_key(1);

        // First access
        cache.record_access(key, 100, AccessType::Get);
        let first_access = cache.get(&key).unwrap().last_accessed;

        // Advance time and access again
        time.advance_time(Duration::from_secs(10));
        cache.record_access(key, 100, AccessType::Put);

        // Should still be one contract, but updated
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.current_bytes(), 100);

        let info = cache.get(&key).unwrap();
        assert_eq!(info.access_type, AccessType::Put);
        assert!(info.last_accessed > first_access);
    }

    #[test]
    fn test_lru_eviction_respects_ttl() {
        // Cache with max 200 bytes, 60 second TTL
        let (mut cache, time) = make_cache(200, Duration::from_secs(60));
        let key1 = make_key(1);
        let key2 = make_key(2);
        let key3 = make_key(3);

        // Add two entries
        cache.record_access(key1, 100, AccessType::Get);
        cache.record_access(key2, 100, AccessType::Get);
        assert_eq!(cache.current_bytes(), 200);

        // Advance time by 30 seconds (under TTL)
        time.advance_time(Duration::from_secs(30));

        // Add third entry - should NOT evict because all entries under TTL
        let result = cache.record_access(key3, 100, AccessType::Get);
        assert!(
            result.evicted.is_empty(),
            "Should not evict entries under TTL"
        );
        assert_eq!(
            cache.len(),
            3,
            "Cache should exceed budget when all under TTL"
        );
        assert!(cache.contains(&key1));
        assert!(cache.contains(&key2));
        assert!(cache.contains(&key3));
    }

    #[test]
    fn test_lru_eviction_after_ttl() {
        // Cache with max 200 bytes, 60 second TTL
        let (mut cache, time) = make_cache(200, Duration::from_secs(60));
        let key1 = make_key(1);
        let key2 = make_key(2);
        let key3 = make_key(3);

        // Add two entries
        cache.record_access(key1, 100, AccessType::Get);
        cache.record_access(key2, 100, AccessType::Get);

        // Advance time past TTL
        time.advance_time(Duration::from_secs(61));

        // Add third entry - should evict key1 (oldest)
        let result = cache.record_access(key3, 100, AccessType::Get);
        assert!(result.is_new);
        assert_eq!(result.evicted, vec![key1]);
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains(&key1));
        assert!(cache.contains(&key2));
        assert!(cache.contains(&key3));
    }

    #[test]
    fn test_access_refreshes_lru_position() {
        let (mut cache, time) = make_cache(200, Duration::from_secs(60));
        let key1 = make_key(1);
        let key2 = make_key(2);
        let key3 = make_key(3);

        // Add two contracts
        cache.record_access(key1, 100, AccessType::Get);
        cache.record_access(key2, 100, AccessType::Get);

        // Access key1 again - should move it to back of LRU
        cache.record_access(key1, 100, AccessType::Subscribe);

        // LRU order should now be [key2, key1]
        let order = cache.keys_lru_order();
        assert_eq!(order, vec![key2, key1]);

        // Advance past TTL and add key3 - should evict key2 (now oldest)
        time.advance_time(Duration::from_secs(61));
        let result = cache.record_access(key3, 100, AccessType::Get);

        assert_eq!(result.evicted, vec![key2]);
        assert!(cache.contains(&key1));
        assert!(!cache.contains(&key2));
        assert!(cache.contains(&key3));
    }

    #[test]
    fn test_touch_refreshes_ttl() {
        let (mut cache, time) = make_cache(200, Duration::from_secs(60));
        let key1 = make_key(1);
        let key2 = make_key(2);
        let key3 = make_key(3);

        // Add two entries
        cache.record_access(key1, 100, AccessType::Get);
        cache.record_access(key2, 100, AccessType::Get);

        // Advance time by 50 seconds
        time.advance_time(Duration::from_secs(50));

        // Touch key1 (simulating UPDATE received)
        cache.touch(&key1);

        // Advance another 15 seconds (key1 now at 15s, key2 at 65s)
        time.advance_time(Duration::from_secs(15));

        // Add key3 - should evict key2 (past TTL), NOT key1 (recently touched)
        let result = cache.record_access(key3, 100, AccessType::Get);

        assert_eq!(
            result.evicted,
            vec![key2],
            "Should evict key2 which is past TTL"
        );
        assert!(
            cache.contains(&key1),
            "key1 should remain (touched recently)"
        );
        assert!(cache.contains(&key3));
    }

    #[test]
    fn test_large_contract_evicts_multiple() {
        let (mut cache, time) = make_cache(300, Duration::from_secs(60));

        let small1 = make_key(1);
        let small2 = make_key(2);
        let small3 = make_key(3);
        let large = make_key(4);

        // Add three small contracts
        cache.record_access(small1, 100, AccessType::Get);
        cache.record_access(small2, 100, AccessType::Get);
        cache.record_access(small3, 100, AccessType::Get);
        assert_eq!(cache.current_bytes(), 300);

        // Advance past TTL
        time.advance_time(Duration::from_secs(61));

        // Add one large contract - should evict two small ones
        let result = cache.record_access(large, 200, AccessType::Put);

        assert_eq!(result.evicted.len(), 2);
        assert_eq!(result.evicted[0], small1); // Oldest first
        assert_eq!(result.evicted[1], small2);
        assert!(!cache.contains(&small1));
        assert!(!cache.contains(&small2));
        assert!(cache.contains(&small3));
        assert!(cache.contains(&large));
    }

    #[test]
    fn test_sweep_expired() {
        let (mut cache, time) = make_cache(200, Duration::from_secs(60));
        let key1 = make_key(1);
        let key2 = make_key(2);
        let key3 = make_key(3);

        // Add three entries (exceeds budget)
        cache.record_access(key1, 100, AccessType::Get);
        cache.record_access(key2, 100, AccessType::Get);
        cache.record_access(key3, 100, AccessType::Get);
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.current_bytes(), 300);

        // Sweep immediately - nothing should be evicted (all under TTL)
        let evicted = cache.sweep_expired(|_| false);
        assert!(evicted.is_empty());
        assert_eq!(cache.len(), 3);

        // Advance past TTL
        time.advance_time(Duration::from_secs(61));

        // Sweep should evict oldest entry to get back under budget
        let evicted = cache.sweep_expired(|_| false);
        assert_eq!(evicted, vec![key1]);
        assert_eq!(cache.current_bytes(), 200);
    }

    #[test]
    fn test_sweep_respects_should_retain() {
        let (mut cache, time) = make_cache(200, Duration::from_secs(60));
        let key1 = make_key(1);
        let key2 = make_key(2);
        let key3 = make_key(3);

        // Add three entries (exceeds budget)
        cache.record_access(key1, 100, AccessType::Get);
        cache.record_access(key2, 100, AccessType::Get);
        cache.record_access(key3, 100, AccessType::Get);

        // Advance past TTL
        time.advance_time(Duration::from_secs(61));

        // Sweep with predicate that retains key1
        let evicted = cache.sweep_expired(|k| *k == key1);

        // key1 should be retained, key2 evicted to get under budget
        assert_eq!(evicted, vec![key2]);
        assert!(cache.contains(&key1));
        assert!(!cache.contains(&key2));
        assert!(cache.contains(&key3));

        // key1 should now be at back of LRU (moved there when retained)
        assert_eq!(cache.current_bytes(), 200);
    }

    #[test]
    fn test_touch_non_existent_is_no_op() {
        let (mut cache, _) = make_cache(1000, Duration::from_secs(60));
        let key = make_key(1);

        // Touch a key that doesn't exist
        cache.touch(&key);

        // Should remain empty
        assert!(cache.is_empty());
        assert!(!cache.contains(&key));
    }

    #[test]
    fn test_access_types() {
        let (mut cache, _) = make_cache(1000, Duration::from_secs(60));
        let key = make_key(1);

        // Test each access type is recorded correctly
        cache.record_access(key, 100, AccessType::Get);
        assert_eq!(cache.get(&key).unwrap().access_type, AccessType::Get);

        cache.record_access(key, 100, AccessType::Put);
        assert_eq!(cache.get(&key).unwrap().access_type, AccessType::Put);

        cache.record_access(key, 100, AccessType::Subscribe);
        assert_eq!(cache.get(&key).unwrap().access_type, AccessType::Subscribe);
    }

    #[test]
    fn test_contract_size_change() {
        let (mut cache, _) = make_cache(1000, Duration::from_secs(60));
        let key = make_key(1);

        // Add contract with initial size
        cache.record_access(key, 100, AccessType::Get);
        assert_eq!(cache.current_bytes(), 100);
        assert_eq!(cache.get(&key).unwrap().size_bytes, 100);

        // Contract state grows
        cache.record_access(key, 200, AccessType::Put);
        assert_eq!(cache.current_bytes(), 200);
        assert_eq!(cache.get(&key).unwrap().size_bytes, 200);

        // Contract state shrinks
        cache.record_access(key, 150, AccessType::Put);
        assert_eq!(cache.current_bytes(), 150);
        assert_eq!(cache.get(&key).unwrap().size_bytes, 150);
    }

    // =========================================================================
    // CWP (Lepus) Tests
    // =========================================================================

    #[cfg(feature = "lepus")]
    mod cwp_tests {
        use super::*;

        fn make_cwp_contract(
            size_bytes: u64,
            last_accessed: Instant,
            bytes_served: u64,
            bytes_consumed: u64,
            deposited_xlm: u64,
            creator_verified: bool,
            subscriber_verified: bool,
        ) -> HostedContract {
            HostedContract {
                size_bytes,
                last_accessed,
                access_type: AccessType::Get,
                commitment: CommitmentState {
                    deposited_xlm,
                    last_oracle_check: None,
                },
                identity: IdentityState {
                    creator_pubkey: None,
                    creator_verified,
                    subscriber_pubkey: None,
                    subscriber_verified,
                    recipient_pubkey: None,
                },
                bytes_served,
                bytes_consumed,
            }
        }

        #[test]
        fn test_recency_score_halflife() {
            let config = CWPConfig::default();
            let now = Instant::now();
            let halflife = Duration::from_secs_f64(config.recency_halflife_secs);
            let contract = make_cwp_contract(1000, now - halflife, 0, 0, 0, false, false);

            let score = contract.recency_score(now, &config);
            // After exactly one half-life, score should be ~0.5
            assert!((score - 0.5).abs() < 0.01, "Expected ~0.5, got {}", score);
        }

        #[test]
        fn test_recency_score_fresh() {
            let config = CWPConfig::default();
            let now = Instant::now();
            let contract = make_cwp_contract(1000, now, 0, 0, 0, false, false);

            let score = contract.recency_score(now, &config);
            assert!(
                (score - 1.0).abs() < 0.001,
                "Just-accessed should be ~1.0, got {}",
                score
            );
        }

        #[test]
        fn test_contribution_score_zero_consumed() {
            let config = CWPConfig::default();
            // bytes_consumed = 0 should not panic (max(0, 1) = 1)
            let contract = make_cwp_contract(1000, Instant::now(), 100, 0, 0, false, false);
            let score = contract.contribution_score(&config);
            // 100 / 1 / 1.5 = 66.67, clamped to 1.0
            assert!(
                (score - 1.0).abs() < 0.001,
                "High ratio should clamp to 1.0, got {}",
                score
            );
        }

        #[test]
        fn test_contribution_score_exceeds_target() {
            let config = CWPConfig::default();
            let contract = make_cwp_contract(1000, Instant::now(), 3000, 1000, 0, false, false);
            let score = contract.contribution_score(&config);
            // ratio = 3.0, target = 1.5, 3.0/1.5 = 2.0 → clamped to 1.0
            assert!(
                (score - 1.0).abs() < 0.001,
                "Exceeding target should clamp to 1.0, got {}",
                score
            );
        }

        #[test]
        fn test_commitment_score_zero_deposit() {
            let config = CWPConfig::default();
            let contract = make_cwp_contract(1000, Instant::now(), 0, 0, 0, false, false);
            let score = contract.commitment_score(&config);
            assert!(
                score.abs() < 0.001,
                "Zero deposit should give 0.0, got {}",
                score
            );
        }

        #[test]
        fn test_identity_score_both_verified() {
            let contract = make_cwp_contract(1000, Instant::now(), 0, 0, 0, true, true);
            let score = contract.identity_score();
            assert!(
                (score - 1.0).abs() < 0.001,
                "Both verified should give 1.0, got {}",
                score
            );
        }

        #[test]
        fn test_identity_score_none_verified() {
            let contract = make_cwp_contract(1000, Instant::now(), 0, 0, 0, false, false);
            let score = contract.identity_score();
            assert!(
                score.abs() < 0.001,
                "None verified should give 0.0, got {}",
                score
            );
        }

        #[test]
        fn test_persistence_score_weighted_sum() {
            let config = CWPConfig::default();
            let now = Instant::now();
            // All factors at 0 except recency (just accessed → ~1.0)
            let contract = make_cwp_contract(1000, now, 0, 0, 0, false, false);
            let score = contract.persistence_score(now, &config);

            // Expected: 0.50*0 + 0.25*0 + 0.15*0 + 0.10*1.0 = 0.10
            assert!(
                (score - 0.10).abs() < 0.01,
                "Only recency should contribute, got {}",
                score
            );
        }

        #[test]
        fn test_cwp_eviction_lowest_score_evicted() {
            let (mut cache, time) = make_cache(200, Duration::from_secs(60));
            let key1 = make_key(1);
            let key2 = make_key(2);
            let key3 = make_key(3);

            // Add two contracts
            cache.record_access(key1, 100, AccessType::Get);
            cache.record_access(key2, 100, AccessType::Get);

            // Give key2 higher contribution score
            cache.record_bytes_served(&key2, 10000);

            // Advance past TTL
            time.advance_time(Duration::from_secs(61));

            // Add key3 — should evict key1 (lower score: no contribution)
            let result = cache.record_access(key3, 100, AccessType::Get);
            assert!(result.is_new);
            assert_eq!(result.evicted, vec![key1]);
            assert!(!cache.contains(&key1));
            assert!(cache.contains(&key2));
            assert!(cache.contains(&key3));
        }

        #[test]
        fn test_cwp_eviction_respects_min_ttl() {
            let (mut cache, time) = make_cache(200, Duration::from_secs(60));
            let key1 = make_key(1);
            let key2 = make_key(2);
            let key3 = make_key(3);

            cache.record_access(key1, 100, AccessType::Get);
            cache.record_access(key2, 100, AccessType::Get);

            // Advance only 30s (under TTL)
            time.advance_time(Duration::from_secs(30));

            // Add key3 — should NOT evict (all under TTL)
            let result = cache.record_access(key3, 100, AccessType::Get);
            assert!(result.evicted.is_empty());
            assert_eq!(cache.len(), 3);
        }

        #[test]
        fn test_cwp_equal_scores_approximates_lru() {
            // When all CWP factors are default (0), recency dominates.
            // Oldest should be evicted first.
            let (mut cache, time) = make_cache(200, Duration::from_secs(60));
            let key1 = make_key(1);
            let key2 = make_key(2);
            let key3 = make_key(3);

            cache.record_access(key1, 100, AccessType::Get);
            time.advance_time(Duration::from_secs(1));
            cache.record_access(key2, 100, AccessType::Get);

            // Both past TTL
            time.advance_time(Duration::from_secs(61));

            // key1 is older → lower recency score → evicted first
            let result = cache.record_access(key3, 100, AccessType::Get);
            assert_eq!(result.evicted, vec![key1]);
        }

        #[test]
        fn test_record_bytes_served_updates_contribution() {
            let (mut cache, _) = make_cache(1000, Duration::from_secs(60));
            let key = make_key(1);

            cache.record_access(key, 100, AccessType::Get);
            cache.record_bytes_served(&key, 500);
            cache.record_bytes_served(&key, 300);

            let contract = cache.get(&key).unwrap();
            assert_eq!(contract.bytes_served, 800);
        }

        #[test]
        fn test_record_bytes_consumed_updates() {
            let (mut cache, _) = make_cache(1000, Duration::from_secs(60));
            let key = make_key(1);

            cache.record_access(key, 100, AccessType::Get);
            cache.record_bytes_consumed(&key, 200);
            cache.record_bytes_consumed(&key, 100);

            let contract = cache.get(&key).unwrap();
            assert_eq!(contract.bytes_consumed, 300);
        }

        #[test]
        fn test_contribution_affects_eviction_order() {
            let (mut cache, time) = make_cache(300, Duration::from_secs(60));
            let key1 = make_key(1); // no contribution
            let key2 = make_key(2); // high contribution
            let key3 = make_key(3); // no contribution

            cache.record_access(key1, 100, AccessType::Get);
            cache.record_access(key2, 100, AccessType::Get);
            cache.record_access(key3, 100, AccessType::Get);

            // Give key2 significant contribution
            cache.record_bytes_served(&key2, 5000);

            // Advance past TTL
            time.advance_time(Duration::from_secs(61));

            // Need to evict to add key4 - should evict key1 or key3 (lowest scores), not key2
            let key4 = make_key(4);
            let result = cache.record_access(key4, 100, AccessType::Get);

            assert!(
                !result.evicted.contains(&key2),
                "High-contribution key2 should survive eviction"
            );
            assert!(cache.contains(&key2));
        }

        // =================================================================
        // Andromica Datapod Validation Tests (Phase 4)
        // =================================================================

        /// Typical Andromica datapod: ~2 KB NINJS JSON metadata per subscriber
        const DATAPOD_SIZE: u64 = 2048;
        /// Deposit amount that saturates commitment for a 2KB datapod.
        /// At density_target 0.001: 2048 * 0.001 = 2.048, deposit 10 → score 1.0
        const DATAPOD_DEPOSIT: u64 = 10;

        /// Helper: set up a datapod contract with commitment + identity.
        fn setup_datapod(
            cache: &mut HostingCache<SharedMockTimeSource>,
            key: ContractKey,
            deposited_xlm: u64,
            creator_verified: bool,
            subscriber_verified: bool,
        ) {
            cache.record_access(key, DATAPOD_SIZE, AccessType::Put);
            cache.update_commitment(&key, deposited_xlm, cache.time_source.now());
            cache.update_identity(
                &key,
                Some([1u8; 32]),
                creator_verified,
                if subscriber_verified {
                    Some([2u8; 32])
                } else {
                    None
                },
                subscriber_verified,
                Some([2u8; 32]),
            );
        }

        /// Helper: set up a spam contract (no deposit, no identity).
        fn setup_spam(cache: &mut HostingCache<SharedMockTimeSource>, key: ContractKey, size: u64) {
            cache.record_access(key, size, AccessType::Get);
        }

        /// §9/§15 core scenario: 10 bespoke datapods survive while 10 spam contracts
        /// are evicted first. Budget fits 15 contracts; all 10 datapods survive.
        #[test]
        fn test_datapod_gallery_persists_over_spam() {
            // Budget: 15 * DATAPOD_SIZE
            let budget = 15 * DATAPOD_SIZE;
            let (mut cache, time) = make_cache(budget, Duration::from_secs(60));

            // 10 bespoke datapods (committed + identity-verified)
            let datapod_keys: Vec<_> = (1..=10).map(|i| make_key(i)).collect();
            for &key in &datapod_keys {
                setup_datapod(&mut cache, key, DATAPOD_DEPOSIT, true, true);
            }

            // 10 spam contracts (no deposit, no identity, same size)
            let spam_keys: Vec<_> = (11..=20).map(|i| make_key(i)).collect();
            for &key in &spam_keys {
                setup_spam(&mut cache, key, DATAPOD_SIZE);
            }

            assert_eq!(cache.len(), 20);

            // Advance past TTL
            time.advance_time(Duration::from_secs(61));

            // Add 5 more contracts to force eviction down to budget
            for i in 21..=25 {
                cache.record_access(make_key(i), DATAPOD_SIZE, AccessType::Get);
            }

            // All 10 datapods should survive
            for &key in &datapod_keys {
                assert!(
                    cache.contains(&key),
                    "Bespoke datapod should survive eviction"
                );
            }
            // At least some spam should be evicted
            let spam_remaining = spam_keys.iter().filter(|k| cache.contains(k)).count();
            assert!(
                spam_remaining < 10,
                "Spam should be evicted before datapods, remaining: {spam_remaining}"
            );
        }

        /// §1 core problem: single-subscriber bespoke datapod survives over
        /// uncommitted contracts with higher contribution/recency.
        #[test]
        fn test_datapod_single_subscriber_not_penalized() {
            // Budget: 4 contracts
            let budget = 4 * DATAPOD_SIZE;
            let (mut cache, time) = make_cache(budget, Duration::from_secs(60));

            // 1 bespoke datapod (committed, single subscriber)
            let datapod = make_key(1);
            setup_datapod(&mut cache, datapod, DATAPOD_DEPOSIT, true, true);

            // 4 uncommitted contracts with high contribution + recency
            let uncommitted: Vec<_> = (2..=5).map(|i| make_key(i)).collect();
            for &key in &uncommitted {
                cache.record_access(key, DATAPOD_SIZE, AccessType::Get);
                cache.record_bytes_served(&key, 10000); // high contribution
            }

            // Advance past TTL
            time.advance_time(Duration::from_secs(61));

            // Force eviction by adding a new contract
            let new_key = make_key(6);
            cache.record_access(new_key, DATAPOD_SIZE, AccessType::Get);

            // Datapod must survive — commitment (50%) dominates contribution (15%)
            assert!(
                cache.contains(&datapod),
                "Committed datapod should survive over uncommitted high-contribution contracts"
            );
        }

        /// §10 two tiers: Tier A (committed+identity) > Tier B (committed only) > Tier C (uncommitted).
        #[test]
        fn test_datapod_two_tier_eviction_ordering() {
            // Budget: 3 contracts — adding a 4th forces eviction of the weakest
            let budget = 3 * DATAPOD_SIZE;
            let (mut cache, time) = make_cache(budget, Duration::from_secs(60));

            let tier_c = make_key(1); // uncommitted
            let tier_b = make_key(2); // committed only
            let tier_a = make_key(3); // committed + identity

            setup_spam(&mut cache, tier_c, DATAPOD_SIZE);
            setup_datapod(&mut cache, tier_b, DATAPOD_DEPOSIT, false, false); // committed, no identity
            setup_datapod(&mut cache, tier_a, DATAPOD_DEPOSIT, true, true); // committed + identity

            // Advance past TTL
            time.advance_time(Duration::from_secs(61));

            // Force eviction: add one more
            let pressure = make_key(4);
            let result = cache.record_access(pressure, DATAPOD_SIZE, AccessType::Get);

            // Tier C (uncommitted) should be evicted first
            assert!(
                result.evicted.contains(&tier_c),
                "Uncommitted tier C should be evicted first"
            );
            assert!(cache.contains(&tier_a), "Tier A should survive");
            assert!(cache.contains(&tier_b), "Tier B should survive");
        }

        /// §3 density normalization: small datapods reach max commitment_score
        /// with modest deposits; large contracts need more.
        #[test]
        fn test_datapod_commitment_density_by_size() {
            let config = CWPConfig::default();
            let now = Instant::now();

            // 2KB datapod with 10 XLM: density = 10 / (2048 * 0.001) = 4.88 → clamped to 1.0
            let small = make_cwp_contract(DATAPOD_SIZE, now, 0, 0, 10, false, false);
            let small_score = small.commitment_score(&config);
            assert!(
                (small_score - 1.0).abs() < 0.001,
                "Small datapod with 10 XLM should have commitment 1.0, got {small_score}"
            );

            // 50KB contract with 10 XLM: density = 10 / (51200 * 0.001) = 0.195
            let large = make_cwp_contract(51200, now, 0, 0, 10, false, false);
            let large_score = large.commitment_score(&config);
            assert!(
                large_score < 0.25,
                "Large contract with same deposit should have low commitment, got {large_score}"
            );
        }

        /// §5 identity only: commitment + identity boundary scores.
        #[test]
        fn test_datapod_identity_without_commitment() {
            let config = CWPConfig::default();
            let now = Instant::now();

            // Identity-only (no deposit): 0.25 * 1.0 = 0.25
            let identity_only = make_cwp_contract(DATAPOD_SIZE, now, 0, 0, 0, true, true);
            let id_score = identity_only.persistence_score(now, &config);
            // Expected: 0.50*0 + 0.25*1.0 + 0.15*0 + 0.10*1.0 = 0.35
            assert!(
                (id_score - 0.35).abs() < 0.02,
                "Identity-only score should be ~0.35, got {id_score}"
            );

            // Commitment-only (no identity): 0.50 * 1.0 = 0.50 + recency
            let commit_only =
                make_cwp_contract(DATAPOD_SIZE, now, 0, 0, DATAPOD_DEPOSIT, false, false);
            let commit_score = commit_only.persistence_score(now, &config);
            // Expected: 0.50*1.0 + 0.25*0 + 0.15*0 + 0.10*1.0 = 0.60
            assert!(
                (commit_score - 0.60).abs() < 0.02,
                "Commitment-only score should be ~0.60, got {commit_score}"
            );

            // Both: 0.50*1.0 + 0.25*1.0 = 0.75 + recency
            let both = make_cwp_contract(DATAPOD_SIZE, now, 0, 0, DATAPOD_DEPOSIT, true, true);
            let both_score = both.persistence_score(now, &config);
            // Expected: 0.50*1.0 + 0.25*1.0 + 0.15*0 + 0.10*1.0 = 0.85
            assert!(
                (both_score - 0.85).abs() < 0.02,
                "Both score should be ~0.85, got {both_score}"
            );
        }

        /// Subscriber verification updates identity score incrementally.
        #[test]
        fn test_datapod_subscriber_verification_updates_score() {
            let (mut cache, _) = make_cache(10000, Duration::from_secs(60));
            let key = make_key(1);

            cache.record_access(key, DATAPOD_SIZE, AccessType::Put);

            // Creator verified only
            cache.update_identity(&key, Some([1u8; 32]), true, None, false, None);
            let score1 = cache.get(&key).unwrap().identity_score();
            assert!(
                (score1 - 0.6).abs() < 0.001,
                "Creator-only identity should be 0.6, got {score1}"
            );

            // Now also subscriber verified
            cache.update_identity(
                &key,
                Some([1u8; 32]),
                true,
                Some([2u8; 32]),
                true,
                Some([2u8; 32]),
            );
            let score2 = cache.get(&key).unwrap().identity_score();
            assert!(
                (score2 - 1.0).abs() < 0.001,
                "Both verified identity should be 1.0, got {score2}"
            );

            // Score jumped by 0.4 (subscriber component)
            assert!(
                (score2 - score1 - 0.4).abs() < 0.001,
                "Subscriber verification should add 0.4, got {}",
                score2 - score1,
            );
        }

        /// Oracle commitment update changes persistence_score by 0.50.
        #[test]
        fn test_datapod_oracle_commitment_updates_score() {
            let (mut cache, _) = make_cache(10000, Duration::from_secs(60));
            let key = make_key(1);
            let config = CWPConfig::default();
            let now = cache.time_source.now();

            cache.record_access(key, DATAPOD_SIZE, AccessType::Put);

            // No commitment
            cache.update_commitment(&key, 0, now);
            let score1 = cache.get(&key).unwrap().persistence_score(now, &config);

            // Add commitment
            cache.update_commitment(&key, DATAPOD_DEPOSIT, now);
            let score2 = cache.get(&key).unwrap().persistence_score(now, &config);

            // Commitment adds 0.50 * 1.0 = 0.50
            let delta = score2 - score1;
            assert!(
                (delta - 0.50).abs() < 0.02,
                "Commitment should add ~0.50, got {delta}"
            );
        }

        /// Full creator lifecycle: PUT → identity → commitment → serve bytes → age → spam flood → survive.
        #[test]
        fn test_datapod_full_lifecycle() {
            // Budget fits datapod + 20 spam without triggering premature eviction
            let budget = 25 * DATAPOD_SIZE;
            let (mut cache, time) = make_cache(budget, Duration::from_secs(60));
            let datapod = make_key(1);

            // PUT 2KB datapod
            cache.record_access(datapod, DATAPOD_SIZE, AccessType::Put);

            // Identity envelope verified (creator + subscriber)
            cache.update_identity(
                &datapod,
                Some([1u8; 32]),
                true,
                Some([2u8; 32]),
                true,
                Some([2u8; 32]),
            );

            // Oracle reports deposit
            cache.update_commitment(&datapod, DATAPOD_DEPOSIT, cache.time_source.now());

            // Node serves bytes
            cache.record_bytes_served(&datapod, 5000);

            // Age 3 days (recency decays)
            time.advance_time(Duration::from_secs(3 * 86400));

            // Verify score is still high despite aging
            let config = CWPConfig::default();
            let now = cache.time_source.now();
            let score = cache.get(&datapod).unwrap().persistence_score(now, &config);
            assert!(
                score > 0.80,
                "Fully committed datapod at 3 days should score >0.80, got {score}"
            );

            // Flood cache with 20 spam contracts (fits within budget, no eviction yet)
            for i in 10..30 {
                cache.record_access(make_key(i), DATAPOD_SIZE, AccessType::Get);
            }
            assert_eq!(cache.len(), 21); // datapod + 20 spam

            // Advance past TTL so ALL contracts (datapod + spam) are eviction-eligible
            time.advance_time(Duration::from_secs(61));

            // Force eviction by adding contracts beyond budget — spam evicted first
            for i in 30..40 {
                cache.record_access(make_key(i), DATAPOD_SIZE, AccessType::Get);
            }

            // Datapod survives all evictions (high CWP score beats spam)
            assert!(
                cache.contains(&datapod),
                "Fully committed datapod should survive spam flood"
            );
        }
    }
}
