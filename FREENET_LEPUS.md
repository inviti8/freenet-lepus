# Lepus: Heavymeta's Freenet Fork — Design Document

> **Lepus** — a Freenet fork purpose-built for bespoke, creator-first content distribution with commitment-weighted persistence.

## Table of Contents

1. [Rationale: Why Fork Freenet](#rationale-why-fork-freenet)
2. [Design Philosophy](#design-philosophy)
3. [Commitment-Weighted Persistence (CWP)](#commitment-weighted-persistence-cwp)
4. [Soroban Commitment Oracle](#soroban-commitment-oracle)
5. [Identity-Aware Caching](#identity-aware-caching)
6. [Unified Key Architecture: One Keypair, One Identity](#unified-key-architecture-one-keypair-one-identity)
7. [Modified Data Structures](#modified-data-structures)
8. [Eviction Algorithm](#eviction-algorithm)
9. [Bespoke Content Model](#bespoke-content-model)
10. [Fallback Behavior: Uncommitted Content](#fallback-behavior-uncommitted-content)
11. [IPFS: Why It Stays As-Is](#ipfs-why-it-stays-as-is)
12. [Anti-Gaming Properties](#anti-gaming-properties)
13. [Persistence Deposit Model (Soroban)](#persistence-deposit-model-soroban)
14. [Network Compatibility & Isolation](#network-compatibility--isolation)
15. [Implementation Roadmap](#implementation-roadmap)
16. [Design Decisions (Resolved)](#design-decisions-resolved)

---

## Rationale: Why Fork Freenet

Freenet (the new Freenet / Locutus) is architecturally aligned with Heavymeta's values — decentralized, censorship-resistant, zero-fee, real-time P2P state sync. Its contract/delegate/UI model maps cleanly to Heavymeta's existing Rust+WASM contract expertise. But Freenet's persistence model has a fundamental problem:

**Freenet's caching algorithm is popularity-based. Popularity rewards mass appeal. Heavymeta's content model rewards bespoke, per-subscriber encrypted distribution.**

These are opposite value systems:

| | Popularity Model (Freenet) | Bespoke Model (Heavymeta) |
|---|---|---|
| Who accesses content? | Many people (the more the better) | One specific subscriber |
| What drives persistence? | Demand volume | Economic commitment |
| What's valuable? | Viral reach | Creator-subscriber relationship |
| Encryption | Unusual (public = popular = persistent) | Default (each subscriber gets unique encrypted copy) |
| Legitimate access pattern | Many nodes, many subnets, high frequency | One node, one subnet, infrequent |

Under Freenet's current algorithm (byte-budget LRU, evict oldest first), a bespoke datapod encrypted for one subscriber scores identically to abandoned spam — both have one subscriber, one access pattern, one ring segment. The algorithm cannot distinguish between "content no one wants" and "content built for exactly one person who paid for it."

**Mitigation layers don't fix this.** Building anti-gaming Soroban contracts and verification daemons on top of a naive caching layer is a permanent cat-and-mouse game. The protocol itself must understand that **economic commitment, not popularity, is the legitimate persistence signal** in a web3 content distribution system.

Lepus makes this change at the source.

---

## Design Philosophy

### Core Principle: Commitment Is the Value Signal

In web2, popularity is a reasonable proxy for value — content that many people access justifies the server costs to host it. This breaks in web3 because:

1. **Demand can be manufactured** (Sybil subscriptions)
2. **Bespoke content is inherently unpopular** (encrypted for one person)
3. **Economic commitment is directly measurable** (XLM deposited on Soroban)

Lepus replaces "how popular is this?" with "has someone committed real resources to keep this alive?" The cost of persistence is explicit and proportional — no free-riding, no gaming through manufactured demand.

### Design Constraints

1. **Bespoke-first**: A datapod encrypted for one subscriber must persist as reliably as a viral public contract with 10,000 subscribers, provided both have equivalent economic backing.

2. **Creator-sovereign**: Creators control their content's lifecycle. Persistence is determined by the creator's economic commitment, not by network popularity dynamics they cannot influence.

3. **Economically rational**: Gaming the system must cost more than the benefit gained. The protocol makes this true structurally, not through policing.

4. **Backwards-compatible for uncommitted content**: Content without economic backing falls back to standard LRU behavior. Lepus is a superset of Freenet's existing model, not a replacement.

5. **Minimal fork surface**: Change only what needs changing (the caching/eviction layer and its inputs). Keep contracts, delegates, delta-sync, ring routing, FrTP, and the WebSocket API identical to upstream Freenet.

---

## Commitment-Weighted Persistence (CWP)

### The Scoring Formula

Every cached contract receives a **persistence score** computed from four factors:

```
persistence_score(contract) =
    W_commitment  × commitment_score  +
    W_identity    × identity_score    +
    W_contribution × contribution_score +
    W_recency     × recency_score
```

**Default weights**:

| Factor | Weight | Rationale |
|--------|--------|-----------|
| `W_commitment` | 0.50 | Economic backing is the strongest anti-gaming signal |
| `W_identity` | 0.25 | Verified creator + subscriber proves legitimacy |
| `W_contribution` | 0.15 | Rewards nodes that serve the network |
| `W_recency` | 0.10 | Basic freshness signal, gentle decay |

When the cache exceeds its byte budget, contracts with the **lowest persistence score** are evicted first — not the oldest.

### Factor 1: Economic Commitment (50%)

```
commitment_score = min(1.0, deposited_xlm / (contract_size_bytes × commitment_density_target))
```

Where:
- `deposited_xlm`: The XLM currently deposited in hvym-freenet-service for this contract key (queried via the Soroban Commitment Oracle)
- `contract_size_bytes`: The contract's state size in bytes
- `commitment_density_target`: A configurable normalization constant (e.g., 0.001 XLM per byte). A 2 KB datapod with 10 XLM deposited: `10 / (2048 × 0.001) = 4.88` → clamped to `1.0`

**Properties**:
- A bespoke 2 KB datapod with modest deposit scores maximum (1.0)
- A 50 MB contract with no deposit scores zero (0.0)
- A spammer creating thousands of contracts needs thousands of persistence deposits — cost scales linearly
- No way to get free persistence — every byte costs proportional XLM

**Zero-commitment behavior**: Contracts with no persistence deposit score 0.0 on this factor but can still persist via the other factors (recency, contribution). This provides the fallback for non-Heavymeta content.

### Factor 2: Identity Verification (25%)

```
identity_score = (creator_verified × 0.6) + (subscriber_verified × 0.4)
```

Where:
- `creator_verified` (0.0 or 1.0): The contract state contains a valid Ed25519 signature from a Stellar public key registered in the Heavymeta creator registry
- `subscriber_verified` (0.0 or 1.0): The subscribing node's Stellar identity matches the datapod's `recipient_public_key`

**For bespoke datapods**: Both creator and subscriber are always verifiable — both Stellar public keys are embedded in the datapod JSON (`creator_public_key`, `recipient_public_key`). Score: 1.0.

**For anonymous/open content**: Neither identity is verifiable. Score: 0.0. Content persists via commitment and contribution instead.

**Verification mechanism**: The Lepus node validates signatures during `ContractPut` and `ContractUpdate`. Creator verification can also be cached — once a contract's creator signature is validated, the result is stored alongside the contract metadata.

### Factor 3: Network Contribution (15%)

```
contribution_score = min(1.0, node_contribution_ratio / contribution_target)

node_contribution_ratio = bandwidth_served / max(bandwidth_consumed, 1)
```

Where:
- `bandwidth_served`: Total bytes this node has served to peers requesting contracts it hosts
- `bandwidth_consumed`: Total bytes this node has consumed via subscriptions and GETs
- `contribution_target`: Configurable normalization (e.g., 1.5 — nodes serving 1.5x what they consume score maximum)

**Properties**:
- Rewards nodes that actively participate in the network
- Free-riding nodes (consume much, serve little) have their hosted contracts score lower
- Not per-contract but per-node — a node's contribution benefits all contracts it hosts

**Bespoke content consideration**: A subscriber node hosting one bespoke datapod might serve little data for that specific contract. But if the node also hosts and serves other content, its overall contribution_score benefits the bespoke datapod too. The incentive is to run a well-connected node, not to free-ride with a single subscription.

### Factor 4: Recency (10%)

```
recency_score = 1.0 / (1.0 + (seconds_since_last_access / recency_halflife))
```

Where:
- `seconds_since_last_access`: Time since the last GET, PUT, or SUBSCRIBE
- `recency_halflife`: Configurable, default **7 days** (604,800 seconds). After 7 days without access, score drops to 0.5. After 14 days, 0.33.

**Deliberately gentle decay.** Unlike Freenet's 8-minute TTL, Lepus uses a halflife measured in days. Bespoke content might be accessed once a week when the subscriber checks for updates — this should not trigger eviction. The recency factor is the weakest signal, serving only as a tiebreaker for content with equivalent commitment, identity, and contribution scores.

---

## Soroban Commitment Oracle

The Oracle is a background service running inside the Lepus node that bridges the Freenet caching layer with the Stellar/Soroban economic layer.

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      Lepus Node                              │
│                                                              │
│  ┌────────────────────┐      ┌───────────────────────────┐  │
│  │    CWP Cache        │◄────│  Soroban Commitment Oracle │  │
│  │                     │     │                            │  │
│  │  persistence_score  │     │  Polls Soroban RPC every   │  │
│  │  per contract       │     │  60s for:                  │  │
│  │                     │     │                            │  │
│  │  Evicts by lowest   │     │  - Active persistence      │  │
│  │  score when over    │     │    deposits for contract   │  │
│  │  budget             │     │  - Creator registration    │  │
│  └────────┬───────────┘     │    status                  │  │
│           │                  │  - Subscriber identity     │  │
│           │                  │    verification            │  │
│           ▼                  │                            │  │
│  ┌────────────────────┐     │  Caches results locally    │  │
│  │  Standard Freenet   │     │  with configurable TTL     │  │
│  │  Components         │     │  (default: 5 min)          │  │
│  │                     │     └───────────────────────────┘  │
│  │  Contracts          │                  │                  │
│  │  Delegates          │                  ▼                  │
│  │  Delta-Sync         │     ┌───────────────────────────┐  │
│  │  Ring Routing       │     │  Soroban RPC               │  │
│  │  FrTP               │     │  (soroban-testnet/mainnet) │  │
│  │  WebSocket API      │     └───────────────────────────┘  │
│  └────────────────────┘                                      │
└─────────────────────────────────────────────────────────────┘
```

### Oracle Responsibilities

1. **Poll hvym-freenet-service** for active persistence deposits matching contract keys in the local cache
2. **Resolve deposit amounts** — for each cached contract, report the total XLM deposited for persistence
3. **Verify creator registration** — check if the contract's creator public key is registered in a Heavymeta creator registry contract
4. **Cache results locally** with a configurable TTL (default 5 minutes) to avoid hammering Soroban RPC
5. **Handle disconnection gracefully** — if Soroban RPC is unreachable, use cached results with extended TTL. Never evict committed content just because the Oracle is temporarily offline.

### Oracle Data Model

```rust
/// Commitment data for a single contract, sourced from Soroban
pub struct CommitmentRecord {
    pub contract_key: ContractKey,
    pub deposited_xlm: u64,             // Total XLM deposited for this contract's persistence
    pub depositor: StellarPublicKey,    // Who deposited (typically the creator)
    pub creator_verified: bool,         // Creator pubkey registered in Heavymeta registry
    pub subscriber_verified: bool,      // Subscribing node matches recipient_public_key
    pub last_verified: Instant,         // When this record was last refreshed from Soroban
    pub verification_ttl: Duration,     // How long to trust this record without refreshing
}

/// Local cache of commitment records
pub struct CommitmentCache {
    records: HashMap<ContractKey, CommitmentRecord>,
    default_ttl: Duration,              // Default: 5 minutes
    offline_ttl: Duration,              // Extended TTL when Soroban is unreachable: 30 minutes
    soroban_rpc_url: String,
    freenet_contract_id: String,        // hvym-freenet-service contract address
}
```

### Polling Strategy

The Oracle does **not** query Soroban for every cache operation. Instead:

1. **Batch refresh**: Every 60 seconds, collect all contract keys in the local cache and batch-query Soroban in a single RPC call (using `get_deposits()` or a filtered view)
2. **On-demand refresh**: When the CWP cache needs a commitment_score for a contract whose `CommitmentRecord` has expired TTL, trigger an immediate single-contract query
3. **Startup hydration**: On node startup, query all active persistence deposits from hvym-freenet-service and pre-populate the CommitmentCache for contracts in the hosting cache

### Graceful Degradation

If Soroban RPC is unreachable:
- Extend all existing `CommitmentRecord` TTLs by `offline_ttl` (30 minutes)
- New contracts without cached records score 0.0 on commitment (fall back to LRU-like behavior)
- Log warnings but do NOT panic or stop the node
- Resume normal polling when RPC comes back

This ensures the node remains functional even if Stellar is having issues.

---

## Identity-Aware Caching

### Creator Verification

When a Lepus node receives a `ContractPut` or `ContractUpdate`:

1. Extract `creator_public_key` from the contract state (datapod JSON)
2. Verify the Ed25519 signature over the state delta using that public key
3. Optionally: check the Heavymeta creator registry on Soroban (via Oracle) to confirm the public key is registered
4. Cache the verification result: `(contract_key, creator_pubkey, verified: bool)`

This verification happens once at ingestion time — subsequent cache scoring reads the cached result.

### Subscriber Verification

When a node subscribes to a contract:

1. The subscribing node presents its Stellar public key as part of the subscription handshake (Lepus protocol extension)
2. The hosting node extracts `recipient_public_key` from the contract state
3. If they match: `subscriber_verified = true`
4. The verification result propagates with the subscription metadata

**For bespoke datapods**: Creator and subscriber are always both verifiable. The datapod contains both public keys, and both can be checked against the Soroban identity layer.

**For open/public content**: `recipient_public_key` is absent or set to a well-known "public" sentinel value. `subscriber_verified` returns true for any subscriber (public content is accessible to all).

### Protocol Extension: Subscription Identity

Lepus extends Freenet's subscription handshake to include an optional Stellar identity field:

```
// Standard Freenet subscription
ContractSubscribe { key: ContractKey }

// Lepus extension
ContractSubscribe {
    key: ContractKey,
    subscriber_identity: Option<StellarPublicKey>,  // NEW
    identity_signature: Option<Ed25519Signature>,    // Proves ownership of the key
}
```

Nodes that don't provide identity still work — they just score 0.0 on subscriber_verified. The extension is backward-compatible: non-Lepus fields are ignored by standard Freenet nodes (though Lepus nodes form their own network — see [Network Compatibility](#network-compatibility--isolation)).

---

## Unified Key Architecture: One Keypair, One Identity

### The Opportunity

Freenet and Stellar both sit on the **Curve25519** family. This means a single Stellar Ed25519 keypair can serve as the universal identity across the entire Heavymeta stack — no separate key management for each system.

### Cryptographic Alignment

| Layer | Freenet Upstream | Stellar | Same Curve? | Same Algorithm? |
|---|---|---|---|---|
| **Signing** | Ed25519 (River, Ghost Keys, delegates) | Ed25519 | Yes | Yes — identical |
| **Key Exchange** | X25519 (transport, ECIES) | X25519 (overlay) | Yes | Yes — identical |
| **Transport Encryption** | ChaCha20Poly1305 | libsodium crypto_box (XSalsa20Poly1305) | N/A | Different cipher, same curve |
| **Hashing** | BLAKE3 | SHA-256 | N/A | Different hash |
| **Underlying Curve** | Curve25519 (Montgomery + Edwards) | Curve25519 (Edwards) | Yes | Yes |

Both systems use `ed25519-dalek` (Freenet's River and Ghost Keys) and the standard Ed25519 key format: 32-byte secret keys, 32-byte public keys, 64-byte signatures.

### The One Wrinkle: Transport vs. Signing Keys

Freenet's **transport layer** (freenet-core) uses X25519 directly for node identity — not Ed25519. A `TransportPublicKey` is an X25519 DH key, not an Ed25519 signing key:

```rust
// freenet-core/crates/core/src/transport/crypto.rs
pub struct TransportPublicKey(x25519_dalek::PublicKey);   // X25519, not Ed25519
pub struct TransportSecretKey(x25519_dalek::StaticSecret); // X25519
```

However, **Ed25519 keys can be mathematically converted to X25519** — this is a well-known property of the Curve25519 family. River (Freenet's reference app) already does this for ECIES encryption, and hvym_stellar already does it for `StellarSharedKey` (ECDH):

```rust
// Ed25519 → X25519 conversion (already proven in both ecosystems)
let ed25519_secret: ed25519_dalek::SigningKey = ...;
let x25519_secret: x25519_dalek::StaticSecret = ed25519_to_x25519(&ed25519_secret);
let x25519_public: x25519_dalek::PublicKey = x25519_dalek::PublicKey::from(&x25519_secret);
```

### Unified Key Model for Lepus

In Lepus, a single Stellar Ed25519 keypair serves all roles:

```
┌─────────────────────────────────────────────────────────┐
│              STELLAR ED25519 KEYPAIR                     │
│              (managed by hvym_stellar)                   │
│                                                          │
│  Secret Key: 32 bytes (Ed25519 SigningKey)               │
│  Public Key: 32 bytes (Ed25519 VerifyingKey)             │
│  Address:    Stellar G... format (base32-encoded)        │
└───────────┬──────────┬──────────┬──────────┬────────────┘
            │          │          │          │
            ▼          ▼          ▼          ▼
     ┌──────────┐ ┌─────────┐ ┌─────────┐ ┌──────────────┐
     │ Stellar  │ │ Lepus   │ │ Lepus   │ │ Bespoke      │
     │ Identity │ │ Contract│ │Transport│ │ Encryption   │
     │          │ │ Signing │ │ Node ID │ │              │
     │ Sign txs │ │ Sign    │ │ Ed25519 │ │ Ed25519 →    │
     │ Sign JWTs│ │ datapod │ │ → X25519│ │ X25519 → DH  │
     │ Auth APIs│ │ deltas  │ │ convert │ │ → shared key │
     └──────────┘ └─────────┘ └─────────┘ └──────────────┘
       Native       Native     Converted    Converted
       Ed25519      Ed25519    to X25519    to X25519
```

### What Each Role Does

**1. Stellar Identity** (native Ed25519)
- Sign Soroban transactions (deposit_persistence, withdraw_persistence, etc.)
- Generate Stellar JWT tokens (tunnel auth, API auth)
- Identify wallets on the Stellar network

**2. Lepus Contract Signing** (native Ed25519)
- Sign datapod deltas published to Lepus contracts
- Creator verification: CWP checks that contract state is signed by the creator's Ed25519 key
- Subscriber identity: proves the subscriber is the intended `recipient_public_key`

**3. Lepus Transport Node ID** (Ed25519 → X25519 conversion)
- Lepus node identity on the P2P network
- Encryption of peer-to-peer messages (FrTP uses X25519 DH → ChaCha20Poly1305)
- The conversion is deterministic — same Ed25519 key always produces the same X25519 key

**4. Bespoke Content Encryption** (Ed25519 → X25519 → ECDH)
- Derive per-subscriber shared keys: `ECDH(creator_x25519, subscriber_x25519)`
- Encrypt/decrypt datapod content and IPFS media
- Same mechanism hvym_stellar already uses via `StellarSharedKey`

### What This Eliminates

| Before (Multiple Keys) | After (Unified) |
|---|---|
| Stellar keypair for wallet/auth | Single Stellar keypair |
| Separate Freenet transport keypair | Derived from Stellar keypair |
| Separate signing key for contracts | Stellar Ed25519 directly |
| Separate ECDH keys for encryption | Derived from Stellar keypair |
| Key sync problem across systems | No sync needed — one key, one identity |

### Fork Modification: Transport Keypair Derivation

In upstream Freenet, `TransportKeypair` is generated randomly and stored in a local config file. In Lepus, it's **derived from the Stellar keypair**:

```rust
// Upstream Freenet: random transport key
impl TransportKeypair {
    pub fn new() -> Self {
        let secret = StaticSecret::random();
        let public = PublicKey::from(&secret);
        Self { public: TransportPublicKey(public), secret: TransportSecretKey(secret) }
    }
}

// Lepus: derive transport key from Stellar Ed25519
impl TransportKeypair {
    pub fn from_stellar(stellar_secret: &ed25519_dalek::SigningKey) -> Self {
        // Convert Ed25519 secret to X25519 secret (standard curve conversion)
        let ed_bytes = stellar_secret.to_bytes();
        let hash = sha2::Sha512::digest(&ed_bytes);
        let mut x_bytes = [0u8; 32];
        x_bytes.copy_from_slice(&hash[..32]);
        x_bytes[0] &= 248;
        x_bytes[31] &= 127;
        x_bytes[31] |= 64;

        let secret = StaticSecret::from(x_bytes);
        let public = PublicKey::from(&secret);
        Self { public: TransportPublicKey(public), secret: TransportSecretKey(secret) }
    }
}
```

This is the **only additional change** to freenet-core's transport layer. The rest of the transport protocol (FrTP encryption, NAT traversal, connection management) works unchanged because it already expects X25519 keys.

### Delegate Key Management

Freenet delegates (the local-only WASM agents) handle secrets through an opaque `SecretsId` system. In Lepus, the Heavymeta delegate stores the Stellar secret key and performs all cryptographic operations:

```rust
// Heavymeta Lepus Delegate (Rust → WASM)
struct HeavymetaDelegate {
    stellar_signing_key: ed25519_dalek::SigningKey,  // The one key
}

impl HeavymetaDelegate {
    fn sign_datapod_delta(&self, delta: &[u8]) -> ed25519_dalek::Signature {
        self.stellar_signing_key.sign(delta)
    }

    fn derive_shared_key(&self, recipient_pubkey: &[u8; 32]) -> [u8; 32] {
        // Ed25519 → X25519 → ECDH (same as hvym_stellar StellarSharedKey)
        let our_x25519 = ed25519_to_x25519(&self.stellar_signing_key);
        let their_x25519 = ed25519_pubkey_to_x25519(recipient_pubkey);
        our_x25519.diffie_hellman(&their_x25519).to_bytes()
    }

    fn stellar_public_key(&self) -> [u8; 32] {
        self.stellar_signing_key.verifying_key().to_bytes()
    }
}
```

The delegate runs in a WASM sandbox — the secret key never leaves the delegate's memory. This is stronger isolation than the current Python-based hvym_stellar, where keys live in the same process as the UI.

### Two-Tier Identity: Funded and Unfunded Keypairs

A Stellar Ed25519 keypair can exist in two states, and this maps naturally to a two-tier identity model on Lepus:

**Funded keypair** (economic participant):
- Stellar address is active on the ledger (someone sent XLM to it)
- Can deposit XLM in hvym-freenet-service for content persistence
- Node ID is linked to a publicly visible Stellar address — but this is not a privacy concern because the address already has on-chain economic activity. If you're depositing XLM, you've chosen to be economically visible.

**Unfunded keypair** (ghost participant):
- Valid Ed25519 identity — can sign, verify, derive shared keys
- Zero on-chain footprint until funded. The address "doesn't exist" on Stellar.
- Can run a Lepus node, subscribe to contracts, receive bespoke datapods
- Cannot participate in economic layer (no XLM to deposit for persistence)
- Functionally equivalent to a Freenet Ghost Key — a cryptographic identity with no public economic trail — but without the blind RSA ceremony. Just generate a keypair and save the seed.

```
┌──────────────────────────────────────────────────────────┐
│              IDENTITY TIERS IN LEPUS                      │
│                                                           │
│  ┌─────────────────────────┐  ┌────────────────────────┐ │
│  │  FUNDED KEYPAIR          │  │  UNFUNDED KEYPAIR       │ │
│  │  (Economic Participant)  │  │  (Ghost Participant)    │ │
│  │                          │  │                         │ │
│  │  ✓ Sign contracts        │  │  ✓ Sign contracts       │ │
│  │  ✓ Verify identity       │  │  ✓ Verify identity      │ │
│  │  ✓ Derive shared keys    │  │  ✓ Derive shared keys   │ │
│  │  ✓ Run Lepus node        │  │  ✓ Run Lepus node       │ │
│  │  ✓ Receive datapods      │  │  ✓ Receive datapods     │ │
│  │  ✓ Deposit XLM           │  │  ✗ No XLM to deposit   │ │
│  │  ✓ Persist content       │  │  ✗ No economic backing  │ │
│  │  ✓ Full CWP scoring      │  │  ✗ No commitment score  │ │
│  │                          │  │                         │ │
│  │  On-chain: VISIBLE       │  │  On-chain: INVISIBLE    │ │
│  │  CWP commitment: YES     │  │  CWP commitment: NO     │ │
│  │  CWP identity: YES       │  │  CWP identity: YES      │ │
│  └─────────────────────────┘  └────────────────────────┘ │
└──────────────────────────────────────────────────────────┘
```

**How this interacts with CWP scoring**:

| CWP Factor | Funded Keypair | Unfunded Keypair |
|---|---|---|
| commitment_score (50%) | Based on deposited XLM | 0.0 (no XLM to deposit) |
| identity_score (25%) | Full score (creator/subscriber verified) | Full score (Ed25519 verification is cryptographic, not economic) |
| contribution_score (15%) | Based on bandwidth served | Based on bandwidth served |
| recency_score (10%) | Based on access time | Based on access time |

An unfunded subscriber receiving a bespoke datapod still gets identity_score credit — the Ed25519 signature verification doesn't check whether the Stellar address is funded. It just checks the math. The `recipient_public_key` in the datapod matches the subscriber's Ed25519 key regardless of funding status.

This means **consumers don't need XLM to receive content** — only creators (who deposit XLM for persistence) need funded addresses. A reader with an unfunded keypair is a ghost on the Stellar ledger but a verified identity on the Lepus network.

### The Ghost Key Parallel

Freenet's Ghost Keys use blind RSA signatures to create anonymous-but-verified identities. The ceremony requires a donation, a blinding server, and unblinding — significant infrastructure.

Stellar's unfunded keypair achieves a similar property with zero infrastructure:
- **Anonymous**: No on-chain record exists
- **Verifiable**: Ed25519 signatures prove ownership of the key
- **Activatable**: Fund the address when you want to participate economically
- **No ceremony**: Just generate a keypair locally

The "donation" in Freenet's Ghost Key model maps to "funding" in Stellar's model — both represent a real-world commitment that activates the identity. But Stellar's version is simpler and doesn't require a centralized blinding server.

### Security Considerations

**Key derivation is deterministic**: The same Stellar secret always produces the same X25519 transport key and the same Lepus node ID. For funded keypairs, this links the node to a public Stellar address — but since the address already has on-chain activity (persistence deposits), there's no additional privacy loss. For unfunded keypairs, the node ID exists only on the Lepus P2P network with no Stellar ledger footprint.

**Key compromise scope**: If the Stellar secret key is compromised, all derived keys are compromised (transport, signing, encryption). This is inherent to unified key models. Mitigation: key rotation support (new Stellar keypair → new transport key → re-sign contracts).

---

## Modified Data Structures

### HostedContract (Extended)

```rust
/// A contract entry in the CWP hosting cache.
/// Replaces the upstream HostedContract with commitment-aware fields.
#[derive(Debug, Clone)]
pub struct HostedContract {
    // --- Upstream fields (unchanged) ---
    pub size_bytes: u64,
    pub last_accessed: Instant,
    pub access_type: AccessType,

    // --- CWP commitment fields ---
    pub commitment: CommitmentState,
    pub identity: IdentityState,

    // --- CWP contribution fields ---
    pub bytes_served: u64,          // Bytes served to peers for this contract
    pub bytes_consumed: u64,        // Bytes consumed (subscriptions, GETs)

    // --- CWP frequency fields ---
    pub access_count: u64,          // Total accesses since first cached
    pub first_accessed: Instant,    // For frequency calculation
}

#[derive(Debug, Clone, Default)]
pub struct CommitmentState {
    pub deposited_xlm: u64,        // From Oracle (persistence deposit)
    pub last_oracle_check: Option<Instant>,
}

#[derive(Debug, Clone, Default)]
pub struct IdentityState {
    pub creator_pubkey: Option<[u8; 32]>,   // Stellar Ed25519 public key
    pub creator_verified: bool,              // Signature validated
    pub subscriber_pubkey: Option<[u8; 32]>, // Intended recipient
    pub subscriber_verified: bool,           // Subscribing node matches
}
```

### CWP Configuration

```rust
/// Configuration for the Commitment-Weighted Persistence algorithm.
/// Weights are sourced from the LepusNetworkConfig Soroban contract
/// (network-wide governance) and refreshed by the Oracle.
#[derive(Debug, Clone)]
pub struct CWPConfig {
    // --- Weights (must sum to 1.0) ---
    // Sourced from LepusNetworkConfig Soroban contract via Oracle.
    // Defaults used only when Oracle is unreachable on first startup.
    pub commitment_weight: f64,       // Default: 0.50
    pub identity_weight: f64,         // Default: 0.25
    pub contribution_weight: f64,     // Default: 0.15
    pub recency_weight: f64,          // Default: 0.10

    // --- Normalization targets ---
    pub commitment_density_target: f64,  // XLM per byte for max score. Default: 0.001
    pub contribution_target: f64,        // Bandwidth ratio for max score. Default: 1.5
    pub recency_halflife_secs: f64,      // Seconds until recency drops to 0.5. Default: 604800 (7 days)

    // --- Cache parameters (node-local, operator configurable) ---
    pub budget_bytes: u64,            // Per-node cache budget. Default: 100 MB (same as upstream)
    pub min_ttl: Duration,            // Minimum time before any eviction. Default: 8 min (same as upstream)

    // --- Oracle parameters ---
    pub oracle_poll_interval: Duration,   // Default: 60 seconds
    pub oracle_cache_ttl: Duration,       // Default: 5 minutes
    pub oracle_offline_ttl: Duration,     // Default: 30 minutes
    pub rpc_endpoints: Vec<String>,       // Multiple Soroban RPC endpoints for consensus
}

impl Default for CWPConfig {
    fn default() -> Self {
        Self {
            commitment_weight: 0.50,
            identity_weight: 0.25,
            contribution_weight: 0.15,
            recency_weight: 0.10,
            commitment_density_target: 0.001,
            contribution_target: 1.5,
            recency_halflife_secs: 604_800.0,
            budget_bytes: 100 * 1024 * 1024,
            min_ttl: Duration::from_secs(480),
            oracle_poll_interval: Duration::from_secs(60),
            oracle_cache_ttl: Duration::from_secs(300),
            oracle_offline_ttl: Duration::from_secs(1800),
            rpc_endpoints: vec!["https://soroban-rpc.heavymeta.io".into()],
        }
    }
}
```

---

## Eviction Algorithm

### Scoring

```rust
impl HostedContract {
    pub fn persistence_score(&self, now: Instant, config: &CWPConfig) -> f64 {
        let commitment = self.commitment_score(config);
        let identity = self.identity_score();
        let contribution = self.contribution_score(config);
        let recency = self.recency_score(now, config);

        config.commitment_weight * commitment
            + config.identity_weight * identity
            + config.contribution_weight * contribution
            + config.recency_weight * recency
    }

    fn commitment_score(&self, config: &CWPConfig) -> f64 {
        if self.size_bytes == 0 {
            return 0.0;
        }
        let density = self.commitment.deposited_xlm as f64
            / (self.size_bytes as f64 * config.commitment_density_target);
        density.min(1.0)
    }

    fn identity_score(&self) -> f64 {
        let creator = if self.identity.creator_verified { 0.6 } else { 0.0 };
        let subscriber = if self.identity.subscriber_verified { 0.4 } else { 0.0 };
        creator + subscriber
    }

    fn contribution_score(&self, config: &CWPConfig) -> f64 {
        let consumed = self.bytes_consumed.max(1) as f64;
        let ratio = self.bytes_served as f64 / consumed;
        (ratio / config.contribution_target).min(1.0)
    }

    fn recency_score(&self, now: Instant, config: &CWPConfig) -> f64 {
        let elapsed = (now - self.last_accessed).as_secs_f64();
        1.0 / (1.0 + elapsed / config.recency_halflife_secs)
    }
}
```

### Eviction

Replaces the upstream LRU scan:

```rust
impl<T: TimeSource> HostingCache<T> {
    /// Evict contracts with the lowest persistence score until under budget.
    /// Respects min_ttl — never evicts contracts younger than min_ttl regardless of score.
    fn evict_to_budget(&mut self, needed_bytes: u64) -> Vec<ContractKey> {
        let mut evicted = Vec::new();
        let now = self.time_source.now();

        while self.current_bytes + needed_bytes > self.budget_bytes {
            // Find the contract with the lowest persistence score
            // that has exceeded min_ttl
            let candidate = self.contracts.iter()
                .filter(|(_, c)| (now - c.last_accessed) >= self.min_ttl)
                .min_by(|(_, a), (_, b)| {
                    let score_a = a.persistence_score(now, &self.cwp_config);
                    let score_b = b.persistence_score(now, &self.cwp_config);
                    score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(key, _)| key.clone());

            match candidate {
                Some(key) => {
                    if let Some(contract) = self.contracts.remove(&key) {
                        self.current_bytes -= contract.size_bytes;
                        evicted.push(key);
                    }
                }
                None => break, // All remaining contracts are within min_ttl
            }
        }

        evicted
    }
}
```

### Performance Note

The upstream LRU scan is O(1) per eviction (pop from front of queue). CWP eviction is O(n) where n = number of cached contracts. For the default 100 MB budget with ~1-10 KB datapods, n could be 10,000+.

**Mitigation**: Maintain a sorted auxiliary structure (e.g., `BTreeMap<OrderedFloat<f64>, ContractKey>`) that tracks persistence scores. Scores are recomputed only when inputs change (Oracle refresh, access, subscription change). Eviction becomes O(log n) — pop from the low end of the sorted map.

---

## Bespoke Content Model

### Why Bespoke Content Is First-Class in Lepus

In Andromica's distribution model, a creator publishing a gallery to 50 subscribers creates 50 distinct Freenet contracts — one per subscriber, each encrypted with a unique ECDH shared key derived from `creator_public_key` + `recipient_public_key`.

```
Creator: Alice (Stellar keypair A)
Subscribers: Bob, Carol, Dave

Alice publishes gallery:
  → Datapod for Bob:   encrypted with ECDH(A, B)  → contract_key_bob
  → Datapod for Carol: encrypted with ECDH(A, C)  → contract_key_carol
  → Datapod for Dave:  encrypted with ECDH(A, D)  → contract_key_dave

Each datapod:
  - ~2 KB of JSON metadata
  - IPFS CIDs referencing encrypted media (media stored in IPFS, not Freenet)
  - Signed by Alice's Stellar key
  - Addressed to one specific subscriber
```

Under upstream Freenet's LRU, all three datapods are equally vulnerable to eviction — they each have one subscriber, one access pattern, low frequency. Under CWP:

```
contract_key_bob:
  commitment_score:   1.0  (Alice deposited XLM via deposit_persistence())
  identity_score:     1.0  (Alice verified as creator, Bob verified as subscriber)
  contribution_score: 0.6  (Bob's node serves other content too)
  recency_score:      0.8  (Bob accessed yesterday)

  persistence_score = 0.50(1.0) + 0.25(1.0) + 0.15(0.6) + 0.10(0.8) = 0.92

vs. spam contract:
  commitment_score:   0.0  (no deposit)
  identity_score:     0.0  (anonymous)
  contribution_score: 0.1  (free-riding node)
  recency_score:      0.9  (recently pushed)

  persistence_score = 0.50(0.0) + 0.25(0.0) + 0.15(0.1) + 0.10(0.9) = 0.105
```

Bob's bespoke datapod scores **8.8x higher** than the spam contract. It will be among the last things evicted, not the first.

### Scaling Properties

A creator with 1,000 subscribers creates 1,000 contracts. At ~2 KB each, that's 2 MB of Freenet state — trivially within the 100 MB per-node budget. The creator deposits XLM via `deposit_persistence()` for each contract key, and every caching node on the subscription tree keeps them alive via CWP.

The cost to the creator scales linearly with subscribers: `total_cost = per_contract_deposit × subscriber_count`. This is economically rational — more subscribers = more revenue for the creator = more budget for persistence deposits. No gaming needed.

A whale trying to persist 1,000 fake contracts needs 1,000 persistence deposits. The cost of gaming equals the cost of legitimate use — there's no shortcut.

---

## Fallback Behavior: Uncommitted Content

Not all content on a Lepus network will be Heavymeta content with Soroban economic backing. Public open-source projects, community forums, experimental apps — these may have no economic commitment.

### Fallback Scoring

For contracts with `commitment_score = 0.0` and `identity_score = 0.0`:

```
persistence_score = 0.15 × contribution_score + 0.10 × recency_score
                  = max 0.25
```

This effectively creates two tiers:

| Tier | Score Range | Content Type |
|------|------------|--------------|
| **Committed** | 0.25 — 1.0 | Heavymeta content with persistence deposits + verified identities |
| **Uncommitted** | 0.0 — 0.25 | Community content, experiments, unverified |

Uncommitted content persists via standard LRU-like behavior (recency + contribution). It's evicted first when cache pressure occurs, but in a low-pressure cache it can survive indefinitely — same as upstream Freenet behavior.

This means Lepus remains useful as a general-purpose Freenet node. It just prioritizes committed content when space is scarce.

### Optional: Community Commitment Pool

For community projects that want persistence without a single creator depositing, a **community commitment pool** contract could allow anyone to deposit XLM toward a contract key. The Oracle would aggregate pool deposits alongside direct deposits.

This is a future extension, not a launch requirement.

---

## IPFS: Why It Stays As-Is

A key insight: **the Lepus datapod IS the content.** IPFS just serves the referenced media (images, video, audio). The datapod is the assembled, encrypted, per-subscriber metadata — and it lives on Lepus, protected by CWP.

### The Asymmetry

| Layer | What It Stores | Size | Persistence Mechanism | Gaming Risk |
|-------|---------------|------|----------------------|-------------|
| **Lepus** | Datapods (encrypted JSON metadata, IPFS CID references) | ~2 KB per subscriber | CWP + persistence deposits | Addressed at protocol level |
| **IPFS** | Media files (images, video, audio) referenced by CIDs | MBs-GBs | hvym-pin-service escrow + Pintheon pinning | Existing model is sufficient |

### Why IPFS Doesn't Need a Fork

1. **The datapod is the gatekeeper.** Without the Lepus datapod, an attacker doesn't know which CIDs to access. The datapod contains the CID references and the decryption context. Disrupting IPFS media without compromising the Lepus datapod achieves nothing — the subscriber still has the datapod with CID references and can re-fetch media later.

2. **IPFS media is encrypted per-subscriber.** Each subscriber's media has unique CIDs (encrypted with a unique ECDH shared key). An attacker can't target "popular" content because there is no popular content — every CID serves exactly one subscriber. This is the same property that makes CWP work for Lepus.

3. **hvym-pin-service already works.** The existing escrow-based pinning model (10-slot board, epoch expiration, staker/slashing) is appropriate for IPFS because the cost dynamics are different — media files are large (MBs), explicit pinning with economic incentives makes sense. The datapod layer (tiny, subscription-tree propagated) needs a different model (CWP), but the media layer doesn't.

4. **Gaming IPFS media doesn't matter.** If someone floods IPFS with junk CIDs, those CIDs aren't referenced by any Lepus datapod. They consume IPFS storage but have no effect on Heavymeta content distribution. The datapod is the source of truth — IPFS is just a CDN for the referenced blobs.

### What This Means for Pintheon

Pintheon continues operating exactly as designed:
- Kubo nodes pin CIDs based on hvym-pin-service escrow
- Standard GC rules apply (pinned = keep, unpinned = GC when full)
- No fork needed, no commitment-weighted GC needed
- The existing hvym-pin-service + hvym_pinner daemon handle IPFS economics

The complexity budget is spent on Lepus (where the novel persistence model lives), not on reinventing IPFS GC.

---

## Anti-Gaming Properties

### The Core Insight: Gaming Costs Money

With CWP + persistence deposits, every gaming vector requires real XLM expenditure proportional to the benefit gained. There are no shortcuts — the protocol makes gaming economically irrational.

### What the Protocol Prevents (No External Policing Needed)

| Attack | Why It Fails Under CWP | Cost to Attacker |
|--------|----------------------|-----------------|
| **Sybil caching** | Creating fake nodes to cache your content doesn't help — CWP scores by economic commitment, not by how many nodes cache it. Other nodes still evict low-scoring content. | Bandwidth + infrastructure for zero benefit |
| **Dummy content flooding** | Flooding the network with junk contracts requires persistence deposits for each one. Without deposits, junk is evicted first under cache pressure. | Linear XLM cost per contract — self-defeating |
| **Manufactured popularity** | Frequency and diversity are only 10% of the CWP score. Commitment (50%) dominates. Faking access patterns changes almost nothing. | Bandwidth for ~2% score improvement |
| **Free-riding persistence** | Contribution score penalizes nodes that consume without serving. | Reduced persistence for hosted content |
| **Self-UPDATE cache refresh** | Still blocked (carried over from upstream). UPDATE cannot add to cache. | N/A — protocol-level block |
| **Fake hosting claims** | For bespoke content, the subscriber IS the natural verifier. If the datapod isn't delivered, the subscriber knows. No external verification daemon needed. | Can't fake what the subscriber experiences directly |
| **Cache thrashing** | Min TTL floor still applies. Rapid put/evict attacks are rate-limited. | Bandwidth cost, no persistence benefit |

### Why No External Policing Is Needed

The pre-CWP design required a complex verification daemon ("Sub Hunter") to police subscriber nodes, flag neglect, slash stakes, and enforce diversity requirements. **CWP eliminates all of this** because:

1. **Persistence is protocol-native.** The cache itself decides what stays based on economic commitment. No external daemon needs to verify "is this node really hosting?"
2. **The subscriber is the verifier.** For bespoke one-to-one content, the subscriber knows immediately whether content is available. If it's not, they stop paying. No third-party policing needed.
3. **Gaming is self-defeating.** Every gaming vector costs money (deposits, bandwidth, infrastructure) with returns that are zero or negative. The attacker is paying to degrade their own position.
4. **No marketplace to manipulate.** Without subscription slots, staking, claiming, and reward distribution, there's no complex economic surface to exploit. The contract is a simple deposit ledger.

---

## Persistence Deposit Model (Soroban)

### The Simplification

The pre-CWP design required a complex Soroban contract (hvym-freenet-service) with subscription slots, staking, claiming, Sub Hunters, flag/slash mechanics, diminishing returns curves, and dynamic slot scaling. CWP makes most of this redundant.

**What CWP replaces:**
- ~~Subscription slots~~ → Every caching node naturally hosts committed content (CWP keeps it alive)
- ~~Subscriber staking~~ → No dedicated subscriber nodes needed; subscription tree handles propagation
- ~~Sub Hunter daemon~~ → The subscriber is the natural verifier for bespoke content
- ~~Flag/slash mechanics~~ → No subscriber nodes to police
- ~~Diversity requirements~~ → Bespoke content is one-to-one by design; CWP scores commitment, not diversity
- ~~Dynamic slot scaling~~ → No slots to scale

**What remains:** A simple persistence deposit ledger. Creator deposits XLM, Oracle reads it, CWP scores content.

### Simplified Soroban Contract: hvym-freenet-service

```rust
/// Minimal Soroban contract for persistence deposits.
/// Replaces the complex subscription marketplace.

#[contract]
pub struct HvymFreenetService;

#[contractimpl]
impl HvymFreenetService {
    /// Creator deposits XLM to persist a Lepus contract.
    /// deposit_xlm: amount of XLM to deposit
    /// contract_key: the Lepus contract key (datapod identifier)
    /// duration_epochs: how many epochs (e.g., 30-day periods) to persist
    pub fn deposit_persistence(
        env: Env,
        creator: Address,
        contract_key: BytesN<32>,
        deposit_xlm: i128,
        duration_epochs: u32,
    ) -> DepositRecord {
        creator.require_auth();
        // Transfer XLM from creator to contract deposit pool
        // Store deposit record: (contract_key → deposited_xlm, expiry_epoch)
        // Emit event: PersistenceDeposited { creator, contract_key, amount, expiry }
    }

    /// Creator withdraws remaining deposit (e.g., content sunset).
    /// Partial withdrawals allowed — reduces persistence score proportionally.
    pub fn withdraw_persistence(
        env: Env,
        creator: Address,
        contract_key: BytesN<32>,
        amount: i128,
    ) {
        creator.require_auth();
        // Verify creator owns the deposit
        // Transfer XLM back to creator
        // Update or remove deposit record
        // Emit event: PersistenceWithdrawn { creator, contract_key, amount }
    }

    /// Creator tops up an existing deposit (extend duration or increase amount).
    pub fn topup_persistence(
        env: Env,
        creator: Address,
        contract_key: BytesN<32>,
        additional_xlm: i128,
    ) {
        creator.require_auth();
        // Add to existing deposit
        // Emit event: PersistenceToppedUp { creator, contract_key, additional }
    }

    /// Query: get deposit info for a contract key.
    /// Called by the Oracle during batch refresh.
    pub fn get_deposit(
        env: Env,
        contract_key: BytesN<32>,
    ) -> Option<DepositRecord> {
        // Return deposit record if exists and not expired
    }

    /// Query: batch get deposits for multiple contract keys.
    /// Efficient for Oracle batch refresh (one RPC call for all cached contracts).
    pub fn get_deposits(
        env: Env,
        contract_keys: Vec<BytesN<32>>,
    ) -> Vec<(BytesN<32>, Option<DepositRecord>)> {
        // Return deposit records for all requested keys
    }

    /// Transfer deposit ownership to a new creator address.
    /// Enables key rotation without losing persistence deposits.
    /// Keys are disposable — this makes rotation cheap and fast.
    pub fn rotate_creator(
        env: Env,
        old_creator: Address,
        new_creator: Address,
        contract_key: BytesN<32>,
    ) {
        old_creator.require_auth();
        // Update deposit record: creator = new_creator
        // Emit event: CreatorRotated { contract_key, old_creator, new_creator }
    }

    /// Epoch tick: expire old deposits, return XLM to creators.
    /// Called periodically (e.g., by a cron-like invocation or by any node).
    pub fn expire_deposits(env: Env) {
        // Find deposits past their expiry_epoch
        // Return deposited XLM to creators
        // Remove expired records
        // Emit events: PersistenceExpired { contract_key }
    }
}

#[contracttype]
pub struct DepositRecord {
    pub creator: Address,
    pub deposited_xlm: i128,
    pub deposit_epoch: u32,
    pub expiry_epoch: u32,
    pub contract_key: BytesN<32>,
}
```

### How Lepus and Soroban Interact

```
CREATOR FLOW:
  Andromica → publish datapod to Lepus → contract_key
  Andromica → deposit_persistence(contract_key, xlm, epochs) on Soroban
  Soroban holds deposit → Oracle detects deposit → CWP scores contract highly
  Every caching node on the subscription tree keeps the datapod alive

CONSUMER FLOW:
  Consumer subscribes to contract_key on Lepus
  Lepus delivers datapod via delta-sync (subscription tree propagation)
  Consumer fetches encrypted media from IPFS via CIDs in datapod
  Consumer decrypts locally with ECDH shared key

RENEWAL FLOW:
  Deposit approaching expiry → Creator calls topup_persistence()
  Oracle detects increased deposit → CWP score remains high
  No disruption to subscribers

SUNSET FLOW:
  Creator calls withdraw_persistence() (or deposit expires)
  Oracle reports deposited_xlm = 0
  commitment_score drops to 0.0
  Contract falls to uncommitted tier → subject to standard eviction
  Subscriber still has cached copy until node evicts it
```

### Why Every Node Earns Naturally

In the old model, dedicated "subscriber nodes" staked XLM and claimed rewards. With CWP + persistence deposits, there are no dedicated subscriber nodes — every Lepus node that caches committed content is earning its keep:

- **Nodes cache content because CWP tells them to.** High-scoring content stays in cache. The node doesn't need to explicitly "subscribe" to earn.
- **The subscription tree handles propagation.** When a subscriber subscribes to a contract, delta-sync pushes updates through the Freenet ring topology. Every node along the path caches the content and scores it via CWP.
- **The deposit pays for network-wide persistence.** The creator's deposit doesn't go to a specific node — it signals to ALL nodes (via the Oracle) that this content is worth keeping. The deposit is the persistence fee, period.

This eliminates the need for a reward distribution mechanism. The "reward" for caching committed content is that your node stays useful and well-scored in the CWP system.

---

## Network Compatibility & Isolation

### Lepus Is a Separate Network

Lepus nodes form their own P2P network, separate from the upstream Freenet network. The reasons:

1. **Protocol extension**: The subscription handshake includes optional Stellar identity fields that upstream nodes don't understand
2. **Different eviction behavior**: Lepus nodes make different caching decisions than upstream nodes. Mixing them in one network would create inconsistent persistence guarantees.
3. **Soroban dependency**: Lepus nodes need Soroban RPC access. Upstream nodes don't.
4. **Controlled network**: Heavymeta can ensure all network participants run Lepus, providing consistent behavior.

### What's Shared With Upstream Freenet

| Component | Shared? | Notes |
|-----------|---------|-------|
| Ring routing | Yes | Same small-world topology and distance metric |
| FrTP (transport) | Yes | Same encrypted UDP with NAT traversal |
| Contract format | Yes | Same Rust -> WASM compilation, same contract interface |
| Delegate format | Yes | Same WASM delegates with same API |
| Delta-sync protocol | Yes | Same summary → delta → merge pipeline |
| WebSocket API | Extended | Same base operations + optional identity fields |
| Caching/eviction | **Modified** | CWP replaces LRU |
| Subscription handshake | **Extended** | Optional Stellar identity |
| Node discovery | **Separate** | Lepus bootstrap nodes, not Freenet bootstrap nodes |

### Upstream Tracking

Lepus should track upstream Freenet releases and merge non-caching changes regularly. The fork surface is intentionally minimal:

- `crates/core/src/ring/hosting/cache.rs` — replaced with CWP implementation
- `crates/core/src/ring/hosting.rs` — extended for Oracle integration
- New module: `crates/core/src/ring/hosting/oracle.rs` — Soroban Commitment Oracle
- New module: `crates/core/src/ring/hosting/identity.rs` — Identity verification
- Subscription message types — extended with optional identity fields

Everything else (contracts, delegates, delta-sync, routing, transport, WebSocket API) remains upstream-compatible.

---

## Implementation Roadmap

### Phase 0: Upstream Familiarization

1. Build freenet-core from source
2. Run a local multi-node Freenet network
3. Deploy a test contract, verify subscription and delta-sync behavior
4. Read and annotate `cache.rs`, `hosting.rs`, `interest.rs` in detail
5. Identify all integration points for CWP

**Deliverable**: Annotated fork point analysis. Confirmed build and test environment.

### Phase 1: CWP Cache (Freenet-Only, No Oracle)

1. Fork freenet-core → lepus-core
2. Replace `HostedContract` with CWP-extended version
3. Implement `persistence_score()` with only recency and contribution factors (commitment and identity set to 0.0 — no Oracle yet)
4. Replace LRU eviction with score-based eviction
5. Add sorted auxiliary structure for O(log n) eviction
6. Test: verify CWP behaves like LRU when all contracts have equal scores
7. Benchmark: measure eviction performance with 10,000+ cached contracts

**Deliverable**: Lepus-core with CWP cache, Oracle-less (contribution + recency only).

### Phase 2: Soroban Commitment Oracle + Persistence Deposits

1. Implement `CommitmentCache` and `CommitmentRecord` types
2. Implement Oracle polling loop (background tokio task)
3. Integrate Oracle with CWP cache — commitment_score reads from CommitmentCache
4. Implement graceful degradation (offline mode with extended TTL)
5. Deploy simplified hvym-freenet-service (persistence deposit ledger) to Soroban testnet
6. Test: `deposit_persistence()` on Soroban testnet, verify Oracle detects deposit and CWP score increases
7. Test: `withdraw_persistence()`, verify score drops
8. Test: deposit expiry via `expire_deposits()`, verify graceful degradation

**Deliverable**: Lepus-core with working Oracle. Persistence deposit model functional end-to-end.

### Phase 3: Identity Integration

1. Implement creator signature verification during ContractPut/Update
2. Extend subscription handshake with optional Stellar identity
3. Implement subscriber verification (match recipient_public_key)
4. Integrate identity scores into CWP
5. Test: publish signed datapod, verify creator_verified = true and score increases
6. Test: subscribe with matching Stellar identity, verify subscriber_verified = true

**Deliverable**: Full CWP with all four factors operational.

### Phase 4: Bespoke Content Validation

1. Deploy test scenario: creator publishes 10 bespoke datapods (one per subscriber)
2. Verify all 10 persist with high CWP scores despite single-subscriber access patterns
3. Deploy spam contracts with no economic backing, verify they're evicted first
4. Stress test: fill cache to budget, verify committed content survives while uncommitted is evicted
5. Measure: persistence duration for bespoke vs. uncommitted content under cache pressure

**Deliverable**: Validated bespoke content model. Benchmarks proving committed content survives.

### Phase 5: Network Bootstrap + End-to-End

1. Set up Lepus bootstrap nodes (separate from upstream Freenet)
2. Configure Lepus-specific network discovery
3. Run multi-node Lepus network
4. End-to-end test: Andromica → publish datapod → `deposit_persistence()` → Lepus propagation → subscriber receives → content persists via CWP
5. Verify IPFS media layer works alongside (Pintheon pinning via existing hvym-pin-service)

**Deliverable**: Functional Lepus test network with full Heavymeta integration. IPFS media serving confirmed.

### Phase 6: Production Hardening

1. Security audit of CWP implementation (especially Oracle trust model)
2. Performance optimization (sorted score structure, batch Oracle queries)
3. Monitoring and alerting (Oracle health, cache pressure, eviction rates)
4. Mainnet deployment of hvym-freenet-service (persistence deposit contract)
5. Lepus mainnet bootstrap
6. Documentation and operator guides

**Deliverable**: Production-ready Lepus network.

---

## Design Decisions (Resolved)

### D1: Oracle Trust Model — Multi-RPC with Heavymeta-Operated Nodes

**Decision**: Support multiple Soroban RPC endpoints. Long-term, Heavymeta will operate its own Stellar RPC nodes.

**Implementation**:
- Oracle accepts a list of RPC endpoints (not just one)
- Consensus mode: query N endpoints, accept result if majority agree
- Heavymeta-operated RPC nodes as primary, public endpoints as fallback
- Node operators can configure their own trusted endpoints
- Aggressive local caching (5-min TTL) reduces RPC dependency

```rust
pub struct OracleConfig {
    pub rpc_endpoints: Vec<String>,        // Multiple RPCs, queried in parallel
    pub consensus_threshold: f64,          // Fraction that must agree (default: 0.5+)
    pub cache_ttl: Duration,               // Default: 5 minutes
    pub offline_ttl: Duration,             // Extended TTL when all RPCs fail: 30 minutes
}
```

### D2: Cache Budget — 100 MB Default, Configurable

**Decision**: Keep 100 MB default (matches upstream Freenet). Make configurable per-node.

**Budget implications of increasing**:
- **Disk I/O**: Negligible for 2 KB datapods. 1 GB = ~500,000 datapods. Disk is cheap.
- **Eviction relevance**: Larger budgets reduce eviction pressure, making CWP less relevant day-to-day. CWP becomes the tiebreaker only during genuine cache pressure.
- **Scoring CPU**: With the sorted auxiliary structure (O(log n)), scoring cost is negligible even at 500K contracts.
- **Oracle load**: More contracts = more batch queries. But batching is efficient — one RPC call for all keys.

**Recommendation**: 100 MB default keeps CWP actively meaningful. Operators with more disk can increase. The algorithm works correctly at any budget size.

### D3: Weight Governance — Soroban Contract as Source of Truth

**Decision**: Create a `LepusNetworkConfig` Soroban contract that stores CWP weights as the network-wide source of truth. Weights are adjustable on-chain.

**Rationale**: Per-node weight configuration creates inconsistency — nodes with different weights make different eviction decisions, leading to unpredictable persistence behavior. A Soroban contract provides:
- **Network-wide consistency**: All Lepus nodes read the same weights
- **Transparent governance**: Weight changes are on-chain, auditable, versioned
- **Hot-adjustable**: No node restarts needed — Oracle polls weights alongside deposits
- **Minimal overhead**: One additional Soroban read per Oracle poll cycle (60s)

```rust
#[contract]
pub struct LepusNetworkConfig;

#[contractimpl]
impl LepusNetworkConfig {
    /// Read current CWP weights. Called by every Lepus node's Oracle.
    pub fn get_weights(env: Env) -> CWPWeights {
        // Returns current network-wide weights
    }

    /// Update weights. Restricted to Heavymeta governance multi-sig.
    pub fn set_weights(
        env: Env,
        authority: Address,
        weights: CWPWeights,
    ) {
        authority.require_auth();
        // Verify authority is governance multi-sig
        // Store new weights
        // Emit event: WeightsUpdated { old, new, epoch }
    }
}

#[contracttype]
pub struct CWPWeights {
    pub commitment_weight: u32,     // Basis points (5000 = 0.50)
    pub identity_weight: u32,       // Basis points (2500 = 0.25)
    pub contribution_weight: u32,   // Basis points (1500 = 0.15)
    pub recency_weight: u32,        // Basis points (1000 = 0.10)
    // Must sum to 10000
}
```

**Oracle integration**: The Oracle polls `LepusNetworkConfig.get_weights()` alongside deposit queries. Weights are cached locally with the same TTL. If the config contract is unreachable, nodes use last-known weights (graceful degradation, same pattern as deposit cache).

### D4: Cross-Network Content — No

**Decision**: No cross-network content references. Lepus and Freenet are separate networks. Content must exist on the network where it's referenced.

If interoperability happens to work naturally at some future point (e.g., contract format compatibility), fine — but no engineering effort is spent to enable it. Lepus governs its own persistence guarantees via CWP, period.

### D5: Contract Size Growth — Let the Market Sort It Out

**Decision**: No special handling. Let market dynamics self-correct.

The CWP `commitment_density_target` normalizes by `contract_size_bytes`, so larger datapods naturally need higher deposits for the same persistence score. As a creator's gallery grows and their datapod size increases, their subscription pricing should naturally adjust upward to cover the increased deposit needed. The market incentive is built into the math:

- Small datapod (2 KB, 10 items) → modest deposit → high CWP score
- Large datapod (20 KB, 1000 items) → 10x deposit needed for same score → creator charges more per subscriber
- Creator revenue grows with subscriber count → deposit budget grows proportionally

No intervention needed. `topup_persistence()` is available if a creator wants to increase their deposit as content grows, but the system doesn't mandate it.

### D6: Key Model — Disposable, Lightweight Keys

**Decision**: Keys should be disposable and easily rotated. They should never hold large XLM amounts.

**Design implications**:
- Persistence deposits are held by the hvym-freenet-service contract, not by the keypair's wallet. The keypair is identity/auth only — not a vault.
- Key rotation should be a cheap, fast operation: generate new keypair, transfer deposit ownership, re-sign contracts.
- The hvym-freenet-service contract needs a `rotate_creator` function:

```rust
/// Transfer deposit ownership to a new creator address.
/// Enables key rotation without losing persistence deposits.
pub fn rotate_creator(
    env: Env,
    old_creator: Address,
    new_creator: Address,
    contract_key: BytesN<32>,
) {
    old_creator.require_auth();
    // Update deposit record: creator = new_creator
    // Emit event: CreatorRotated { contract_key, old_creator, new_creator }
}
```

- Subscribers need re-encrypted datapods (new ECDH shared key from new keypair). The creator re-publishes to Lepus with the new key. Subscription tree propagates the update via delta-sync.
- Wallet funding should be minimal — just enough XLM for Soroban transaction fees. The deposits are locked in the contract, not sitting in the wallet.

### D7: Ghost Participant Abuse — 50 Subscription Hard Cap

**Decision**: Unfunded keypairs are limited to **50 active subscriptions**. Funded keypairs have no cap.

**Rationale**: 50 subscriptions is more than enough for a legitimate consumer (most people don't follow 50 creators). An attacker generating millions of unfunded keypairs can only subscribe to 50 contracts each — and those subscriptions score 0.0 on commitment (50% weight), so the content is evicted first under cache pressure anyway. The cap prevents the bandwidth/churn cost of a ghost flood while CWP handles the persistence side.

**Implementation**: Lepus protocol enforces the cap at the subscription handshake:

```rust
// During ContractSubscribe processing
if !subscriber_is_funded(&subscriber_identity) {
    let active_count = self.subscriptions_for_identity(&subscriber_identity);
    if active_count >= MAX_GHOST_SUBSCRIPTIONS {  // 50
        return Err(SubscriptionError::GhostCapReached);
    }
}
// Funded keypairs: no cap applied
```

**Properties**:
- Funded keypairs bypass the cap entirely — if you have XLM, you've proven economic commitment
- The cap is per-identity (Stellar public key), not per-IP — NAT-friendly
- 50 is generous for consumers, prohibitive for attackers (need 20x more keypairs to match one funded identity's throughput)
- Cap value could be governed via `LepusNetworkConfig` if tuning is needed post-launch

### D8: Deposit Pricing — Market-Driven

**Decision**: Market-driven. Creators choose their deposit amount; CWP scores proportionally.

- No contract-enforced pricing tiers. The only constraint is a dust minimum (prevents Soroban storage waste).
- Higher deposit = higher persistence score = stronger guarantee against eviction.
- Creators price their subscriptions to cover deposit costs + margin. The market determines the equilibrium.
- CWP's commitment_density_target serves as a soft benchmark: at `0.001 XLM/byte`, a 2 KB datapod reaches maximum commitment_score with just ~2 XLM. But creators can deposit more for safety margin, or less if they're comfortable with a lower score.

### D9: Name — Lepus

**Decision**: **Lepus** is the official name.

The Freenet logo is a rabbit. Lepus (the hare constellation / genus of true hares) is a natural fit. Alternatives considered: Jack Rabbit, Darko. Keeping Lepus.

### D10: Governance Model — Core Team, Then DAO

**Decision**: Heavymeta core team multi-sig controls `LepusNetworkConfig` at launch. Migrate to DAO governance structure as the network matures.

**Phased approach**:
1. **Launch**: Core team multi-sig (2-of-3 or 3-of-5). Fast iteration on weight tuning as the network finds its equilibrium.
2. **Growth**: Add trusted node operators to the multi-sig. Expand to 5-of-9 or similar.
3. **Maturity**: Transition to DAO structure — weight changes proposed and voted on by stakeholders (node operators, creators, or token holders if governance tokens exist).

The `LepusNetworkConfig` contract already supports any `authority` address — swapping from a multi-sig to a DAO contract address requires no code change, just a governance transition transaction.

### D11: Deposit Dust Minimum — Cover Rent + Margin

**Decision**: Minimum persistence deposit = Soroban storage rent for the `DepositRecord` lifetime + a small margin.

**Rationale**: Each `DepositRecord` consumes ~200 bytes of Soroban contract storage. Storage has real rent costs. The minimum ensures every deposit at least pays for its own existence on-chain, preventing dust spam that wastes contract storage.

**Implementation**:
```rust
pub fn deposit_persistence(/* ... */) -> DepositRecord {
    let min_deposit = env.storage().rent_for_bytes(200) * duration_epochs + DUST_MARGIN;
    if deposit_xlm < min_deposit {
        panic!("Deposit below minimum: {} < {}", deposit_xlm, min_deposit);
    }
    // ...
}
```

The margin is small — enough that the contract isn't losing money on storage, but low enough that small creators aren't priced out. Exact margin TBD during testnet, but targeting "negligible for any real use case."

---

> **All design questions resolved.** No open questions remain. The Lepus design is ready for implementation planning.

---

## References

### Freenet Upstream
- [Freenet Core (GitHub)](https://github.com/freenet/freenet-core)
- [Hosting Cache Implementation](https://github.com/freenet/freenet-core/blob/main/crates/core/src/ring/hosting/cache.rs)
- [Hosting Manager](https://github.com/freenet/freenet-core/blob/main/crates/core/src/ring/hosting.rs)
- [Interest Manager](https://github.com/freenet/freenet-core/blob/main/crates/core/src/ring/interest.rs)
- [Mitigating Sybil Attacks](https://freenet.org/news/456-mitigating-sybil-attacks-in-freenet/)
- [The Persistence of Memory in Freenet (Clarke et al.)](https://www.researchgate.net/publication/2883610_The_Persistence_of_Memory_in_Freenet)
- [Transport Crypto (freenet-core)](https://github.com/freenet/freenet-core/blob/main/crates/core/src/transport/crypto.rs)
- [River Crypto Values](https://github.com/freenet/river) — Ed25519 signing + X25519 ECIES
- [Ghost Key Library](https://crates.io/crates/ghostkey_lib) — Ed25519 + Blind RSA

### Heavymeta
- [FREENET_RESEARCH.md](./FREENET_RESEARCH.md) — Freenet research and hvym-freenet-service design
- hvym-pin-service: `pintheon_contracts/hvym-pin-service/src/`
- hvym_pinner: `hvym_pinner/src/hvym_pinner/`
- Glasswing/Andromica: `glasswing/`
- hvym_stellar: `hvym_stellar/`
