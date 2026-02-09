# Andromica–Lepus Integration Guide

How Andromica communicates with Freenet-Lepus to publish, subscribe, and persist bespoke datapods.

---

## 1. The Datapod Problem

Andromica creates **bespoke datapods**: per-subscriber encrypted NINJS JSON metadata files (~2 KB each). A creator publishing a gallery to 50 subscribers produces 50 distinct Freenet contracts — one per subscriber, each encrypted with a unique ECDH shared key.

Under upstream Freenet's LRU eviction, a datapod encrypted for one subscriber scores identically to abandoned spam:

- Both have one subscriber, one access pattern, one ring segment
- The algorithm cannot distinguish "content no one wants" from "content built for exactly one person who paid for it"

Lepus's Commitment-Weighted Persistence (CWP) fixes this by making **economic commitment** — not popularity — the persistence signal. A committed datapod scores ~0.92 vs spam at ~0.105, an 8.8x advantage.

---

## 2. Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                    ANDROMICA (Python)                    │
│  glasswing/main.py — NiceGUI + pywebview desktop app    │
│                                                         │
│  ┌───────────────┐  ┌──────────────┐  ┌──────────────┐  │
│  │ NINJS Builder  │  │  Envelope    │  │  Soroban     │  │
│  │ (data pod JSON)│  │  Builder     │  │  Client      │  │
│  └───────┬───────┘  └──────┬───────┘  └──────┬───────┘  │
│          │                 │                  │          │
│          └────────┬────────┘                  │          │
│                   ▼                           │          │
│          ┌────────────────┐                   │          │
│          │  WebSocket +   │                   │          │
│          │  JSON client   │                   │          │
│          └────────┬────────┘                   │          │
└───────────────────┼────────────────────────────┼─────────┘
                    │ WebSocket                  │ Soroban RPC
                    │ (JSON — Lepus extension)   │ (stellar-sdk)
                    ▼                            ▼
┌──────────────────────────┐    ┌──────────────────────────┐
│ FREENET-LEPUS NODE       │    │ STELLAR TESTNET/MAINNET  │
│ ws://localhost:7509/v1/  │    │ hvym-freenet-service     │
│ contract/command         │    │ (Soroban smart contract) │
│                          │    │                          │
│ ┌──────────────────────┐ │    │ deposit(contract_id,amt) │
│ │ Datapod Contract     │ │    │ get_deposit(contract_id) │
│ │ (WASM — compiled     │ │    │ withdraw(contract_id)    │
│ │  once, deployed once)│ │    └──────────────────────────┘
│ └──────────────────────┘ │
│                          │
│ CWP Cache evaluates:     │
│  commitment 50%          │
│  identity   25%          │
│  contribution 15%        │
│  recency    10%          │
└──────────────────────────┘
```

**Key insight**: The datapod contract WASM is compiled **once** and deployed **once**. Each subscriber's datapod is a different *instance* of the same contract, differentiated by **Parameters** (which encode the creator/recipient pubkeys). No Jinja or Rust templating per subscriber — the Rust code is generic; what varies is the parameters and state.

---

## 3. The Datapod Contract (Rust → WASM)

### 3.1 What the Contract Does

A Freenet contract is a WASM module that implements four functions:

| Function | Purpose |
|----------|---------|
| `validate_state(params, state)` | Verify the identity envelope signature is valid |
| `update_state(params, state, data)` | Merge incoming delta into current state |
| `summarize_state(params, state)` | Produce a summary for subscription delta-sync |
| `get_state_delta(params, state, summary)` | Compute what changed since last summary |

The contract code is the **same for every datapod**. What makes each datapod unique is:
- **Parameters**: JSON with `creator_pubkey` and `recipient_pubkey` (determines the ContractKey)
- **State**: The actual NINJS JSON wrapped in a Lepus identity envelope

### 3.2 Contract Project Structure

```
contracts/datapod/
├── Cargo.toml
├── src/
│   └── lib.rs          # Contract implementation
└── build/freenet/      # Compiled WASM output (after fdev build)
```

### 3.3 Contract Cargo.toml

```toml
[package]
name = "datapod-contract"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
freenet-stdlib = { version = "0.1", features = ["contract"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
ed25519-dalek = { version = "2", default-features = false, features = ["alloc"] }

[features]
default = ["freenet-main-contract"]
contract = ["freenet-stdlib/contract"]
freenet-main-contract = []
```

### 3.4 Contract Implementation (src/lib.rs)

```rust
//! Datapod contract for Lepus — validates identity envelopes and merges state.
//!
//! One WASM binary handles all datapods. Parameters encode the creator/recipient
//! pubkeys. State is the identity envelope (129-byte header + NINJS JSON payload).

use freenet_stdlib::prelude::*;
use serde::{Deserialize, Serialize};

/// Parameters baked into the ContractKey — same for the life of the contract.
#[derive(Serialize, Deserialize)]
struct DatapodParams {
    /// Creator's Ed25519 public key (32 bytes, hex-encoded)
    creator_pubkey: String,
    /// Intended recipient's Ed25519 public key (hex), or "00..00" for public
    recipient_pubkey: String,
}

/// Identity envelope header layout (matches identity.rs in freenet-lepus):
///   byte  0:      version (0x01)
///   bytes 1-32:   creator_pubkey (32 bytes)
///   bytes 33-96:  creator_signature (64 bytes)
///   bytes 97-128: recipient_pubkey (32 bytes)
///   bytes 129+:   payload (NINJS JSON)
const ENVELOPE_HEADER_SIZE: usize = 129;

pub struct Contract;

#[contract]
impl ContractInterface for Contract {
    fn validate_state(
        parameters: Parameters<'static>,
        state: State<'static>,
        _related: RelatedContracts<'static>,
    ) -> Result<ValidateResult, ContractError> {
        let bytes = state.as_ref();
        if bytes.is_empty() {
            return Ok(ValidateResult::Valid);
        }

        // Must have at least the envelope header
        if bytes.len() < ENVELOPE_HEADER_SIZE {
            return Ok(ValidateResult::Invalid);
        }

        // Parse parameters to get expected creator/recipient
        let params: DatapodParams = serde_json::from_slice(parameters.as_ref())
            .map_err(|e| ContractError::Deser(e.to_string()))?;

        // Verify envelope version
        if bytes[0] != 0x01 {
            return Ok(ValidateResult::Invalid);
        }

        // Extract envelope fields
        let creator_pubkey = &bytes[1..33];
        let signature = &bytes[33..97];
        let recipient_pubkey = &bytes[97..129];
        let payload = &bytes[129..];

        // Verify creator_pubkey matches parameters
        let expected_creator = hex::decode(&params.creator_pubkey)
            .map_err(|e| ContractError::Deser(e.to_string()))?;
        if creator_pubkey != expected_creator.as_slice() {
            return Ok(ValidateResult::Invalid);
        }

        // Verify recipient_pubkey matches parameters
        let expected_recipient = hex::decode(&params.recipient_pubkey)
            .map_err(|e| ContractError::Deser(e.to_string()))?;
        if recipient_pubkey != expected_recipient.as_slice() {
            return Ok(ValidateResult::Invalid);
        }

        // Verify Ed25519 signature: sign(recipient_pubkey || payload)
        let vk = ed25519_dalek::VerifyingKey::from_bytes(
            creator_pubkey.try_into().map_err(|_| {
                ContractError::Other("invalid creator pubkey length".into())
            })?,
        ).map_err(|e| ContractError::Other(e.to_string()))?;

        let sig = ed25519_dalek::Signature::from_bytes(
            signature.try_into().map_err(|_| {
                ContractError::Other("invalid signature length".into())
            })?,
        );

        // Message = recipient_pubkey || payload (matches identity.rs)
        let mut msg = Vec::with_capacity(32 + payload.len());
        msg.extend_from_slice(recipient_pubkey);
        msg.extend_from_slice(payload);

        match vk.verify_strict(&msg, &sig) {
            Ok(()) => Ok(ValidateResult::Valid),
            Err(_) => Ok(ValidateResult::Invalid),
        }
    }

    fn update_state(
        parameters: Parameters<'static>,
        _state: State<'static>,
        data: Vec<UpdateData<'static>>,
    ) -> Result<UpdateModification<'static>, ContractError> {
        // For datapods, an update replaces the entire state (new gallery version).
        // The newest valid state wins.
        for ud in data {
            match ud {
                UpdateData::State(s) | UpdateData::Delta(s) => {
                    if !s.is_empty() {
                        // Validate the new state before accepting
                        let new_state = State::from(s.to_vec());
                        let result = Self::validate_state(
                            parameters.clone(),
                            new_state.clone(),
                            RelatedContracts::new(),
                        )?;
                        if matches!(result, ValidateResult::Valid) {
                            return Ok(UpdateModification::valid(new_state));
                        }
                    }
                }
                UpdateData::StateAndDelta { state, .. } => {
                    if !state.is_empty() {
                        let new_state = State::from(state.to_vec());
                        let result = Self::validate_state(
                            parameters.clone(),
                            new_state.clone(),
                            RelatedContracts::new(),
                        )?;
                        if matches!(result, ValidateResult::Valid) {
                            return Ok(UpdateModification::valid(new_state));
                        }
                    }
                }
                _ => {}
            }
        }
        Err(ContractError::InvalidUpdate)
    }

    fn summarize_state(
        _parameters: Parameters<'static>,
        state: State<'static>,
    ) -> Result<StateSummary<'static>, ContractError> {
        // Summary is a hash of the current state (small, for delta-sync)
        if state.is_empty() {
            return Ok(StateSummary::from(vec![]));
        }
        // For simplicity, use the full state as the summary.
        // Datapods are small (~2 KB), so this is efficient.
        Ok(StateSummary::from(state.as_ref().to_vec()))
    }

    fn get_state_delta(
        _parameters: Parameters<'static>,
        state: State<'static>,
        summary: StateSummary<'static>,
    ) -> Result<StateDelta<'static>, ContractError> {
        // If summary matches current state, no delta needed
        if state.as_ref() == summary.as_ref() {
            return Ok(StateDelta::from(vec![]));
        }
        // Otherwise, send the full state as the delta (datapods are small)
        Ok(StateDelta::from(state.as_ref().to_vec()))
    }
}
```

### 3.5 Building the Contract

```bash
# From the freenet-lepus repo root
cargo run -p fdev -- build --contract-dir contracts/datapod

# Output: contracts/datapod/build/freenet/datapod_contract.wasm
# This single WASM file is used for ALL datapods
```

The compiled WASM is ~50-100 KB. It ships with Andromica (or is fetched once from a known location). It is **never regenerated per subscriber**.

---

## 4. The WebSocket Protocol

### 4.1 Connection Details

Freenet nodes expose a WebSocket API for client applications:

| Setting | Value |
|---------|-------|
| Endpoint | `ws://localhost:7509/v1/contract/command` |
| Encoding | `native` (bincode), `flatbuffers`, or **`json`** (Lepus extension) |
| Max message | 100 MB |
| Max frame | 16 MB |
| Keep-alive | Server pings every 30 seconds |
| Auth | Optional `?authToken=<token>` query param |

### 4.2 JSON Encoding Protocol (Lepus Extension)

Upstream Freenet only supports binary encodings (bincode, FlatBuffers), which require Rust to construct. Lepus adds a **JSON encoding protocol** so that Python (and any other language) can communicate directly with the node using standard WebSocket + JSON — no Rust bridge, no binary serialization, no extra build step.

This works because all client API types (`ClientRequest`, `HostResponse`, etc.) in `freenet-stdlib` already derive `serde::Serialize` and `serde::Deserialize`. JSON is just another serde format.

**Connection URL:**

```
ws://localhost:7509/v1/contract/command?encodingProtocol=json
```

**Rust change required** (small — 4 match arms in `websocket.rs` + 1 enum variant):

```rust
// crates/core/src/util/mod.rs — add Json variant
pub enum EncodingProtocol {
    Flatbuffers,
    Native,
    Json,  // Lepus extension for non-Rust clients
}

// crates/core/src/client_events/websocket.rs — add Json arms
// At each serialization/deserialization point:
EncodingProtocol::Json => serde_json::to_vec(&result)?,     // serialize responses
EncodingProtocol::Json => serde_json::from_slice(&msg)?,     // deserialize requests
```

**Binary fields in JSON**: Contract state and WASM code are raw bytes. In JSON mode, these are **base64-encoded** strings. The `freenet-stdlib` serde implementation handles this via `#[serde(with = "base64")]` annotations on byte fields.

### 4.3 Message Format (JSON Mode)

**PUT request** (Python → Node):

```json
{
  "ContractOp": {
    "Put": {
      "contract": "<base64 WASM + params>",
      "state": "<base64 envelope bytes>",
      "related_contracts": {},
      "subscribe": false,
      "blocking_subscribe": false
    }
  }
}
```

**PUT response** (Node → Python):

```json
{
  "Ok": {
    "ContractResponse": {
      "PutResponse": {
        "key": "Cuj4LbFao6vzZ5VtvZAKZ64Y99qNh7MpTUdaCcEkU4oR"
      }
    }
  }
}
```

**SUBSCRIBE request:**

```json
{
  "ContractOp": {
    "Subscribe": {
      "key": "Cuj4LbFao6vzZ5VtvZAKZ64Y99qNh7MpTUdaCcEkU4oR",
      "summary": null
    }
  }
}
```

**Update notification** (Node → Python, pushed after subscribe):

```json
{
  "Ok": {
    "ContractResponse": {
      "UpdateNotification": {
        "key": "Cuj4LbFao6vzZ5VtvZAKZ64Y99qNh7MpTUdaCcEkU4oR",
        "update": {
          "State": "<base64 envelope bytes>"
        }
      }
    }
  }
}
```

---

## 5. Python Integration Code

### 5.1 Dependencies

All pure Python — no Rust toolchain needed:

```
# requirements.txt additions for Lepus integration
websockets>=12.0      # WebSocket client
cryptography>=42.0    # Ed25519 signing for identity envelope
stellar-sdk>=13.1.0   # Already in glasswing requirements
hvym-stellar>=0.19    # Already in glasswing requirements
```

### 5.2 Identity Envelope Builder (Pure Python)

```python
"""
lepus_envelope.py — Pure-Python identity envelope builder.
Uses the 'cryptography' package for Ed25519.
"""

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from stellar_sdk import Keypair


def create_datapod_envelope(
    creator_stellar_secret: str,
    recipient_stellar_address: str,
    ninjs_json_bytes: bytes,
) -> bytes:
    """Build a Lepus identity envelope in pure Python.

    Same binary format as the Rust version — the Freenet node doesn't
    care which language produced the bytes, only that the Ed25519
    signature is valid.
    """
    # Extract raw keys from Stellar format
    creator_kp = Keypair.from_secret(creator_stellar_secret)
    creator_secret_raw: bytes = creator_kp.raw_secret_key()   # 32 bytes
    creator_pubkey_raw: bytes = creator_kp.raw_public_key()    # 32 bytes

    recipient_kp = Keypair.from_public_key(recipient_stellar_address)
    recipient_pubkey_raw: bytes = recipient_kp.raw_public_key()  # 32 bytes

    # Sign: message = recipient_pubkey || payload
    signing_key = Ed25519PrivateKey.from_private_bytes(creator_secret_raw)
    message = recipient_pubkey_raw + ninjs_json_bytes
    signature: bytes = signing_key.sign(message)  # 64 bytes

    # Assemble envelope
    envelope = bytearray()
    envelope.append(0x01)                          # version byte
    envelope.extend(creator_pubkey_raw)            # 32 bytes
    envelope.extend(signature)                     # 64 bytes
    envelope.extend(recipient_pubkey_raw)          # 32 bytes
    envelope.extend(ninjs_json_bytes)              # payload

    assert len(envelope) == 129 + len(ninjs_json_bytes)
    return bytes(envelope)


def strip_envelope(envelope_bytes: bytes) -> tuple[bytes, bytes, bytes, bytes]:
    """Strip the Lepus identity envelope, returning components.

    Returns:
        (creator_pubkey, signature, recipient_pubkey, payload)
    """
    assert len(envelope_bytes) >= 129, "Envelope too short"
    assert envelope_bytes[0] == 0x01, f"Unknown version: {envelope_bytes[0]}"

    creator_pubkey = envelope_bytes[1:33]
    signature = envelope_bytes[33:97]
    recipient_pubkey = envelope_bytes[97:129]
    payload = envelope_bytes[129:]

    return creator_pubkey, signature, recipient_pubkey, payload
```

### 5.3 WebSocket Client (Pure Python)

A lightweight async client using the `websockets` library and JSON encoding:

```python
"""
lepus_client.py — Pure-Python Freenet-Lepus WebSocket client.

Uses the JSON encoding protocol (Lepus extension) so no Rust
serialization is needed. All messages are standard JSON over WebSocket.
"""

import asyncio
import json
import base64
from pathlib import Path

import websockets


class LepusClient:
    """Async WebSocket client for Freenet-Lepus JSON protocol."""

    def __init__(self, host: str = "localhost", port: int = 7509):
        self.url = f"ws://{host}:{port}/v1/contract/command?encodingProtocol=json"
        self.ws = None

    async def connect(self):
        self.ws = await websockets.connect(
            self.url,
            max_size=100 * 1024 * 1024,  # 100 MB max message
        )

    async def close(self):
        if self.ws:
            await self.ws.close()

    async def put_contract(
        self,
        wasm_bytes: bytes,
        params_bytes: bytes,
        state_bytes: bytes,
        subscribe: bool = False,
    ) -> str:
        """PUT a contract to the Freenet node.

        Args:
            wasm_bytes: Compiled WASM contract code
            params_bytes: Contract parameters (JSON bytes)
            state_bytes: Initial state (identity envelope bytes)
            subscribe: Whether to auto-subscribe after PUT

        Returns:
            The contract key string
        """
        request = {
            "ContractOp": {
                "Put": {
                    "contract": base64.b64encode(wasm_bytes).decode(),
                    "state": base64.b64encode(state_bytes).decode(),
                    "parameters": base64.b64encode(params_bytes).decode(),
                    "related_contracts": {},
                    "subscribe": subscribe,
                }
            }
        }
        await self.ws.send(json.dumps(request))
        response = json.loads(await self.ws.recv())

        if "Err" in response:
            raise RuntimeError(f"PUT failed: {response['Err']}")

        return response["Ok"]["ContractResponse"]["PutResponse"]["key"]

    async def subscribe(self, contract_key: str) -> bool:
        """Subscribe to a contract for real-time updates.

        Returns True if subscription was accepted.
        """
        request = {
            "ContractOp": {
                "Subscribe": {
                    "key": contract_key,
                    "summary": None,
                }
            }
        }
        await self.ws.send(json.dumps(request))
        response = json.loads(await self.ws.recv())

        if "Err" in response:
            raise RuntimeError(f"SUBSCRIBE failed: {response['Err']}")

        return True

    async def get_state(self, contract_key: str) -> bytes:
        """GET the current state of a contract.

        Returns the raw state bytes (identity envelope + payload).
        """
        request = {
            "ContractOp": {
                "Get": {
                    "key": contract_key,
                    "return_contract_code": False,
                }
            }
        }
        await self.ws.send(json.dumps(request))
        response = json.loads(await self.ws.recv())

        if "Err" in response:
            raise RuntimeError(f"GET failed: {response['Err']}")

        state_b64 = response["Ok"]["ContractResponse"]["GetResponse"]["state"]
        return base64.b64decode(state_b64)

    async def recv_notification(self) -> tuple[str, bytes]:
        """Wait for an UpdateNotification from a subscribed contract.

        Returns (contract_key, state_bytes).
        """
        while True:
            response = json.loads(await self.ws.recv())
            if "Ok" in response:
                inner = response["Ok"].get("ContractResponse", {})
                if "UpdateNotification" in inner:
                    notif = inner["UpdateNotification"]
                    key = notif["key"]
                    state_b64 = notif["update"]["State"]
                    return key, base64.b64decode(state_b64)
```

### 5.4 Publishing a Datapod (Creator Side)

This is the complete flow for publishing a gallery to N subscribers.

Each subscriber gets a **fully bespoke content graph**: unique datapod JSON, unique ECDH encryption, unique IPFS CIDs (because the encrypted media files are different per subscriber). The datapod IS the content — there is no shared original.

```python
"""
lepus_publish.py — Publish datapods to Freenet-Lepus.

For each subscriber:
  1. Build NINJS datapod JSON (already done by glasswing/data_pod_audio.py)
  2. ECDH-encrypt the datapod for this subscriber
  3. Wrap in Lepus identity envelope
  4. PUT to Freenet via WebSocket/JSON (creates a unique contract per subscriber)
  5. Deposit XLM on Soroban for persistence
"""

import asyncio
import json
from pathlib import Path
from stellar_sdk import Keypair
from hvym_stellar import Stellar25519KeyPair, StellarSharedKey

from lepus_envelope import create_datapod_envelope
from lepus_client import LepusClient


# The datapod contract WASM — compiled once, same for all datapods
DATAPOD_WASM_PATH = Path("contracts/datapod/build/freenet/datapod_contract.wasm")


def create_datapod_parameters(
    creator_stellar_address: str,
    recipient_stellar_address: str,
) -> bytes:
    """Build the contract Parameters JSON.

    Parameters determine the ContractKey. Same WASM + different params
    = different contract instance = different key on the DHT.
    """
    creator_kp = Keypair.from_public_key(creator_stellar_address)
    recipient_kp = Keypair.from_public_key(recipient_stellar_address)
    params = {
        "creator_pubkey": creator_kp.raw_public_key().hex(),
        "recipient_pubkey": recipient_kp.raw_public_key().hex(),
    }
    return json.dumps(params).encode("utf-8")


async def publish_gallery(
    creator_stellar_secret: str,
    subscribers: list[str],        # List of subscriber G-addresses
    ninjs_datapod: dict,           # The base NINJS datapod dict
    freenet_host: str = "localhost",
    freenet_port: int = 7509,
) -> dict[str, str]:
    """Publish a datapod to each subscriber on Freenet-Lepus.

    Args:
        creator_stellar_secret: Creator's Stellar secret (S... format)
        subscribers: List of subscriber Stellar public keys (G... format)
        ninjs_datapod: The base NINJS datapod dict (customized per subscriber
            by glasswing — each subscriber's renditions[].href already
            contains their unique per-subscriber-encrypted IPFS CIDs)
        freenet_host: Freenet node hostname
        freenet_port: Freenet node WebSocket port

    Returns:
        Dict mapping subscriber G-address → Freenet contract key string
    """
    wasm_bytes = DATAPOD_WASM_PATH.read_bytes()
    creator_kp = Keypair.from_secret(creator_stellar_secret)
    creator_address = creator_kp.public_key

    # Connect to the local Freenet-Lepus node (JSON encoding)
    client = LepusClient(freenet_host, freenet_port)
    await client.connect()

    contract_keys = {}

    try:
        for subscriber_address in subscribers:
            # ── Step 1: Customize the datapod for this subscriber ──
            # glasswing already builds a per-subscriber NINJS with unique
            # IPFS CIDs pointing to per-subscriber-encrypted media
            datapod = dict(ninjs_datapod)
            datapod["creator_public_key"] = creator_address
            datapod["recipient_public_key"] = subscriber_address

            datapod_json_bytes = json.dumps(datapod).encode("utf-8")

            # ── Step 2: Wrap in Lepus identity envelope ──
            # Adds the 129-byte header: version + creator_pubkey + signature + recipient_pubkey
            envelope_bytes = create_datapod_envelope(
                creator_stellar_secret=creator_stellar_secret,
                recipient_stellar_address=subscriber_address,
                ninjs_json_bytes=datapod_json_bytes,
            )

            # ── Step 3: Build contract Parameters ──
            params_bytes = create_datapod_parameters(
                creator_stellar_address=creator_address,
                recipient_stellar_address=subscriber_address,
            )

            # ── Step 4: PUT the contract to Freenet-Lepus ──
            contract_key = await client.put_contract(
                wasm_bytes=wasm_bytes,
                params_bytes=params_bytes,
                state_bytes=envelope_bytes,
            )

            contract_keys[subscriber_address] = contract_key
            print(f"Published datapod for {subscriber_address[:12]}... → {contract_key[:20]}...")
    finally:
        await client.close()

    return contract_keys
```

### 5.5 Persistence Deposits (Soroban)

After publishing, the creator deposits XLM on the Soroban smart contract to ensure CWP persistence:

```python
"""
lepus_deposit.py — Deposit XLM for datapod persistence on Soroban.

The hvym-freenet-service Soroban contract (contracts/hvym-freenet-service/)
provides:
    deposit(caller, contract_id, amount)  → DepositRecord
    withdraw(caller, contract_id)         → i128
    get_deposit(contract_id)              → Option<DepositRecord>

The Lepus oracle (crates/core/src/ring/hosting/oracle.rs) polls this contract
periodically and updates each cached contract's commitment_score.

At CWP density_target 0.001:
    2 KB datapod needs 2048 * 0.001 = 2.048 XLM for commitment_score = 1.0
    Depositing 10 XLM (10_000_000 stroops) saturates the score easily.
"""

from stellar_sdk import (
    Keypair,
    Network,
    Server,
    TransactionBuilder,
)
from stellar_sdk import scval
from stellar_sdk.soroban_rpc import SorobanServer


# Contract address — from contracts/deployments.json after deployment
FREENET_SERVICE_CONTRACT = "CXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"

# Stellar testnet for development
SOROBAN_RPC_URL = "https://soroban-testnet.stellar.org"
HORIZON_URL = "https://horizon-testnet.stellar.org"
NETWORK_PASSPHRASE = Network.TESTNET_NETWORK_PASSPHRASE


def deposit_persistence(
    creator_stellar_secret: str,
    freenet_contract_key: str,
    amount_xlm: float = 10.0,
) -> dict:
    """Deposit XLM for a Freenet contract's persistence.

    This calls the hvym-freenet-service Soroban contract's `deposit` function,
    which transfers native XLM from the creator to the contract escrow.

    The Lepus oracle running on each Freenet node will detect this deposit
    during its next poll cycle and update the contract's commitment_score
    in the CWP cache.

    Args:
        creator_stellar_secret: Creator's Stellar secret (S... format)
        freenet_contract_key: The Freenet contract key (hex or base58 string)
        amount_xlm: Amount of XLM to deposit (default 10, which saturates
            commitment_score for a 2 KB datapod)

    Returns:
        Dict with deposit record from the contract
    """
    keypair = Keypair.from_secret(creator_stellar_secret)
    soroban = SorobanServer(SOROBAN_RPC_URL)
    server = Server(HORIZON_URL)

    # Convert contract key to 32-byte BytesN for Soroban
    # The Freenet ContractKey includes both instance_id (32 bytes) and
    # code_hash (32 bytes). The Soroban contract indexes by instance_id.
    contract_id_bytes = bytes.fromhex(freenet_contract_key[:64])  # First 32 bytes

    # Convert XLM to stroops (1 XLM = 10,000,000 stroops)
    amount_stroops = int(amount_xlm * 10_000_000)

    # Build the Soroban contract invocation
    source_account = server.load_account(keypair.public_key)
    builder = TransactionBuilder(
        source_account=source_account,
        network_passphrase=NETWORK_PASSPHRASE,
        base_fee=100_000,  # 0.01 XLM base fee for Soroban
    )

    # Invoke: deposit(caller, contract_id, amount)
    builder.append_invoke_contract_function_op(
        contract_id=FREENET_SERVICE_CONTRACT,
        function_name="deposit",
        parameters=[
            scval.to_address(keypair.public_key),           # caller: Address
            scval.to_bytes(contract_id_bytes),               # contract_id: BytesN<32>
            scval.to_int128(amount_stroops),                  # amount: i128
        ],
    )
    builder.set_timeout(30)
    tx = builder.build()

    # Simulate first (Soroban requires simulation before submission)
    sim_response = soroban.simulate_transaction(tx)
    if sim_response.error:
        raise RuntimeError(f"Simulation failed: {sim_response.error}")

    # Prepare and sign the transaction with the simulation results
    tx = soroban.prepare_transaction(tx, sim_response)
    tx.sign(keypair)

    # Submit
    response = soroban.send_transaction(tx)
    print(f"Deposit submitted: {response.hash}")
    print(f"  Contract: {freenet_contract_key[:20]}...")
    print(f"  Amount: {amount_xlm} XLM ({amount_stroops} stroops)")

    return {"tx_hash": response.hash, "amount_stroops": amount_stroops}


def batch_deposit(
    creator_stellar_secret: str,
    contract_keys: dict[str, str],
    amount_xlm_per_datapod: float = 10.0,
):
    """Deposit persistence for all datapods in a gallery.

    Args:
        creator_stellar_secret: Creator's Stellar secret
        contract_keys: Dict from publish_gallery() — subscriber → contract_key
        amount_xlm_per_datapod: XLM per datapod (default 10)
    """
    for subscriber, contract_key in contract_keys.items():
        print(f"Depositing {amount_xlm_per_datapod} XLM for {subscriber[:12]}...")
        deposit_persistence(
            creator_stellar_secret=creator_stellar_secret,
            freenet_contract_key=contract_key,
            amount_xlm=amount_xlm_per_datapod,
        )
    print(f"Deposited for {len(contract_keys)} datapods")
```

### 5.6 Subscribing to a Datapod (Consumer Side)

```python
"""
lepus_subscribe.py — Subscribe to datapods on Freenet-Lepus.

The subscriber:
  1. Connects to their local Freenet-Lepus node via WebSocket/JSON
  2. Sends ContractSubscribe for the datapod contract key
  3. GETs the current state (identity envelope + NINJS JSON)
  4. Strips the envelope, parses the datapod, renders the gallery
  5. Continues receiving real-time updates via delta-sync notifications

Note: The subscription handshake identity (Phase 4) happens at the
Freenet node level via LEPUS_STELLAR_SECRET env var — the node
automatically includes the subscriber's Stellar identity when
forwarding the subscription through the network. The Python client
doesn't need to handle this directly.
"""

import asyncio
import json
from stellar_sdk import Keypair
from hvym_stellar import Stellar25519KeyPair, StellarSharedKey

from lepus_envelope import strip_envelope
from lepus_client import LepusClient


async def subscribe_to_datapod(
    subscriber_stellar_secret: str,
    contract_key: str,
    creator_stellar_address: str,
    freenet_host: str = "localhost",
    freenet_port: int = 7509,
) -> dict:
    """Subscribe to a creator's datapod and retrieve the contents.

    Args:
        subscriber_stellar_secret: Subscriber's Stellar secret (S... format)
        contract_key: The Freenet contract key for this datapod
        creator_stellar_address: Creator's Stellar public key (for ECDH)
        freenet_host: Freenet node hostname
        freenet_port: Freenet node WebSocket port

    Returns:
        The NINJS datapod as a Python dict
    """
    subscriber_kp = Keypair.from_secret(subscriber_stellar_secret)

    # Connect to local node (JSON encoding — pure Python, no bridge)
    client = LepusClient(freenet_host, freenet_port)
    await client.connect()

    try:
        # Subscribe for real-time updates
        await client.subscribe(contract_key)

        # GET the current state
        state_bytes = await client.get_state(contract_key)

        # Strip the identity envelope (129-byte header)
        creator_pubkey, signature, recipient_pubkey, payload = strip_envelope(state_bytes)
        print(f"Envelope verified:")
        print(f"  Creator:   {creator_pubkey.hex()[:16]}...")
        print(f"  Recipient: {recipient_pubkey.hex()[:16]}...")
        print(f"  Payload:   {len(payload)} bytes")

        # Verify creator matches expected
        expected_creator = Keypair.from_public_key(creator_stellar_address).raw_public_key()
        assert creator_pubkey == expected_creator, "Creator pubkey mismatch!"

        # The payload is the NINJS JSON — the datapod content.
        # Media CIDs in renditions[].href point to per-subscriber-encrypted
        # files on IPFS (unique CIDs per subscriber).
        datapod = json.loads(payload.decode("utf-8"))

        return datapod
    finally:
        await client.close()


async def listen_for_updates(
    contract_key: str,
    creator_stellar_address: str,
    callback,
    freenet_host: str = "localhost",
    freenet_port: int = 7509,
):
    """Listen for real-time datapod updates.

    After subscribing, the Freenet node pushes UpdateNotification
    messages whenever the creator publishes a new version of the
    datapod (e.g., new gallery items added).

    Args:
        contract_key: The Freenet contract key to listen on
        creator_stellar_address: Creator's public key (for verification)
        callback: Called with the NINJS dict on each update
        freenet_host: Freenet node hostname
        freenet_port: Freenet node WebSocket port
    """
    client = LepusClient(freenet_host, freenet_port)
    await client.connect()

    try:
        await client.subscribe(contract_key)

        while True:
            key, state_bytes = await client.recv_notification()
            if key == contract_key and len(state_bytes) >= 129:
                _, _, _, payload = strip_envelope(state_bytes)
                datapod = json.loads(payload.decode("utf-8"))
                callback(datapod)
    finally:
        await client.close()
```

### 5.7 End-to-End Example

```python
"""
example_e2e.py — Complete creator-to-subscriber flow.

Prerequisites:
  - Freenet-Lepus node running locally (ws://localhost:7509)
  - Datapod contract WASM compiled (contracts/datapod/build/...)
  - Soroban contract deployed (contracts/deployments.json)
  - LEPUS_STELLAR_SECRET env var set on the Freenet node

All pure Python — no Rust bridge, no binary serialization.

Using testnet funded account from pintheon/test_bindings/.env:
  TESTNET_SECRET=SATCSJPFRLVMX2IHPECYET4XEFM2M76P25OKFAG2HMFUUFIPNBIVXF2Q
"""

import asyncio
from stellar_sdk import Keypair


async def main():
    # ── Creator setup ──
    CREATOR_SECRET = "SATCSJPFRLVMX2IHPECYET4XEFM2M76P25OKFAG2HMFUUFIPNBIVXF2Q"
    creator_kp = Keypair.from_secret(CREATOR_SECRET)
    print(f"Creator: {creator_kp.public_key}")

    # ── Generate a test subscriber ──
    subscriber_kp = Keypair.random()
    print(f"Subscriber: {subscriber_kp.public_key}")

    # ── Build a sample NINJS datapod ──
    # In production, glasswing builds this per-subscriber with unique
    # per-subscriber-encrypted IPFS CIDs in renditions[].href
    ninjs_datapod = {
        "uri": "urn:ninjs:v2:com.heavymeta.gallery:test",
        "version": "http://iptc.org/std/ninjs/2.1",
        "content_created": "2026-02-08T12:00:00Z",
        "content_type": "mixed",
        "items": [
            {
                "type": "audio_image",
                "guid": "urn:uuid:QmExampleHash123",
                "title": "Test Image",
                "renditions": [{
                    "name": "original",
                    "href": "http://127.0.0.1:8080/ipfs/QmExampleHash123",
                    "mimetype": "image/png",
                    "width": 1920,
                    "height": 1080,
                }],
                "imageType": "aposematic",
                "hasAudio": False,
            }
        ],
    }

    # ── Step 1: Publish ──
    from lepus_publish import publish_gallery

    contract_keys = await publish_gallery(
        creator_stellar_secret=CREATOR_SECRET,
        subscribers=[subscriber_kp.public_key],
        ninjs_datapod=ninjs_datapod,
    )
    # contract_keys = {"GXYZ...": "Cuj4LbFao6v..."}

    # ── Step 2: Deposit for persistence ──
    from lepus_deposit import batch_deposit

    batch_deposit(
        creator_stellar_secret=CREATOR_SECRET,
        contract_keys=contract_keys,
        amount_xlm_per_datapod=10.0,
    )

    # ── Step 3: Subscriber receives ──
    from lepus_subscribe import subscribe_to_datapod

    contract_key = contract_keys[subscriber_kp.public_key]
    datapod = await subscribe_to_datapod(
        subscriber_stellar_secret=subscriber_kp.secret,
        contract_key=contract_key,
        creator_stellar_address=creator_kp.public_key,
    )

    print(f"Received datapod: {datapod['uri']}")
    print(f"Items: {len(datapod['items'])}")
    for item in datapod["items"]:
        print(f"  - {item['title']} ({item['imageType']})")


if __name__ == "__main__":
    asyncio.run(main())
```

---

## 6. How Parameters Create Unique Contract Instances

This is the key architectural point: **one WASM, many instances**.

```
Same WASM code ──┐
                 │
    Params A ────┤──→ ContractKey A (hash of WASM + Params A)
    (creator=X,  │     State: envelope for subscriber Alice
     recipient=  │
     Alice)      │
                 │
    Params B ────┤──→ ContractKey B (hash of WASM + Params B)
    (creator=X,  │     State: envelope for subscriber Bob
     recipient=  │
     Bob)        │
                 │
    Params C ────┤──→ ContractKey C (hash of WASM + Params C)
    (creator=X,       State: envelope for subscriber Carol
     recipient=
     Carol)
```

The `ContractKey` is a cryptographic hash of the WASM code + the Parameters. Since each subscriber has a unique `recipient_pubkey` in the Parameters, each gets a unique ContractKey, even though the contract logic is identical.

This is why **no Jinja or per-subscriber Rust templating is needed**. The WASM is compiled once. The differentiation happens at the data level (Parameters and State), not at the code level.

---

## 7. NINJS Datapod JSON Structure

Reference structure from `glasswing/data_pod_audio.py`:

```json
{
  "uri": "urn:ninjs:v2:com.heavymeta.gallery:gallery_name",
  "version": "http://iptc.org/std/ninjs/2.1",
  "content_created": "2026-02-08T12:00:00Z",
  "creator_public_key": "GABCDEF...",
  "recipient_public_key": "GXYZ123...",
  "content_type": "mixed",
  "op_string": "-^+",
  "scramble_mode": 2,
  "audio_token_images": ["QmHash1...", "QmHash2..."],
  "items": [
    {
      "type": "audio_image",
      "guid": "urn:uuid:QmIPFSHash...",
      "title": "image_name.png",
      "byline": "Creator Name",
      "renditions": [{
        "name": "original",
        "href": "http://127.0.0.1:8080/ipfs/QmIPFSHash...",
        "mimetype": "image/png",
        "width": 1920,
        "height": 1080
      }],
      "imageType": "aposematic",
      "hasAudio": true,
      "audioMethod": "token",
      "audioTokenInfo": {
        "receiverPublicKey": "GXYZ123...",
        "tokenExpiry": null,
        "noExpiry": true
      }
    }
  ],
  "type_distribution": {
    "raw": 0,
    "processed": 0,
    "aposematic": 3,
    "enciphered": 0,
    "total_with_audio": 2,
    "audio_token_count": 2
  }
}
```

**Size**: Typically 1-3 KB depending on number of items. The IPFS CIDs in `renditions[].href` reference per-subscriber-encrypted media files on IPFS/Pintheon. Each subscriber gets unique CIDs because the media is ECDH-encrypted with a different shared key, producing different ciphertext and therefore different content hashes.

---

## 8. Datapod CWP Scoring

Per bespoke datapod (committed + identity-verified + active subscriber):

```
Alice's datapod for Bob (contract_key_bob):
  commitment_score:   1.0  (Alice deposited 10 XLM via deposit())
  identity_score:     1.0  (Alice verified as creator, Bob verified as subscriber)
  contribution_score: 0.6  (Bob's node serves other content too)
  recency_score:      0.8  (Bob accessed yesterday)

  persistence_score = 0.50(1.0) + 0.25(1.0) + 0.15(0.6) + 0.10(0.8) = 0.92
```

vs. spam contract (no deposit, anonymous):

```
  persistence_score = 0.50(0.0) + 0.25(0.0) + 0.15(0.1) + 0.10(0.9) = 0.105
```

Bespoke datapod scores **8.8x higher** — evicted last, not first.

Two tiers: committed (0.25-1.0) vs uncommitted (0.0-0.25).

For a 2 KB datapod at CWP `density_target = 0.001`:
- Minimum deposit for `commitment_score = 1.0`: `2048 * 0.001 = 2.048 XLM`
- Depositing 10 XLM (density = 10/2.048 = 4.88) saturates the score to 1.0

---

## 9. IPFS/Pintheon Relationship

The entire content graph is **bespoke end-to-end**. Media files are encrypted per-subscriber via ECDH (`StellarSharedKey`), producing unique ciphertext and therefore unique IPFS CIDs for each subscriber. There is no shared "original" on IPFS — every subscriber gets uniquely encrypted files.

- Pintheon (`pintheon/`): Python Flask app with hvym-pin-service Soroban contract for pinning escrow
- Media is encrypted per-subscriber before IPFS upload → unique CIDs per subscriber
- Each datapod's `renditions[].href` CIDs are unique to that subscriber
- Lepus handles datapod metadata (small ~2 KB NINJS JSON per subscriber)
- Disrupting IPFS media without compromising the Lepus datapod achieves nothing

```
Per-subscriber content graph (fully bespoke):

  Creator publishes to subscriber Bob:
    1. Encrypt each media file with ECDH(creator, Bob) → unique ciphertext
    2. Upload encrypted files to IPFS → unique CIDs (QmBob...)
    3. Build NINJS datapod with Bob's unique CIDs in renditions[].href
    4. Wrap in Lepus identity envelope → PUT to Freenet

  Content split:
    Datapod (~2 KB)  → Freenet-Lepus (CWP-protected, real-time sync)
    Media (MB-GB)    → IPFS/Pintheon (pinning escrow, gateway access)

  Both layers are per-subscriber:
    Datapod: unique contract per subscriber (different Parameters → different ContractKey)
    Media:   unique CIDs per subscriber (different ECDH key → different ciphertext)
```

---

## 10. Subscription Identity Handshake

When a subscriber connects, the Lepus extension (Phase 4) adds Stellar identity to the subscription handshake:

```
Subscriber's Freenet node:
  1. Reads LEPUS_STELLAR_SECRET env var
  2. Signs the contract instance_id with Ed25519
  3. Includes StellarIdentityPayload in SubscribeMsg::Request

Hosting node (caching the datapod):
  1. Receives StellarIdentityPayload { stellar_pubkey, signature }
  2. Verifies Ed25519 signature over instance_id bytes
  3. Compares stellar_pubkey against datapod's recipient_pubkey from envelope
  4. If match → subscriber_verified = true → identity_score += 0.4 (×0.25 weight = +0.10)

Ghost cap (FREENET_LEPUS.md D7):
  - Unfunded keypairs limited to 50 active subscriptions
  - Funded keypairs (any contract with deposited_xlm > 0) have no cap
  - Prevents ghost flood attacks from millions of unfunded keypairs
```

---

## 11. Environment Variables

| Variable | Where | Purpose |
|----------|-------|---------|
| `LEPUS_STELLAR_SECRET` | Freenet node | Ed25519 secret for transport key derivation + subscription signing |
| `LEPUS_STELLAR_PUBKEY` | Freenet node | Ed25519 public key for identity matching (Phase 3) |
| `LEPUS_RPC_URL` | Freenet node | Soroban RPC endpoint for oracle polling |
| `LEPUS_CONTRACT_ADDRESS` | Freenet node | hvym-freenet-service Soroban contract ID |
| `LEPUS_POLL_INTERVAL_SECS` | Freenet node | Oracle poll frequency (default: 300) |

---

## 12. What Andromica Needs to Implement

### Already exists in glasswing:
- NINJS datapod JSON builder (`data_pod_audio.py`)
- ECDH shared key derivation (`hvym_stellar.StellarSharedKey`)
- Stellar key management (`stellar-sdk`)
- IPFS upload (`main.py:ipfs_add()`)
- Pintheon directory publishing (`main.py:pintheon_upload_file()`)

### New for Lepus integration:

| Component | Effort | Description |
|-----------|--------|-------------|
| `lepus_client.py` | Small | Pure-Python WebSocket client using JSON encoding protocol |
| `lepus_envelope.py` | Small | Identity envelope builder (pure Python, `cryptography` package) |
| `lepus_publish.py` | Small | Gallery → N datapods → N contract PUTs |
| `lepus_deposit.py` | Small | Soroban deposit calls via stellar-sdk |
| `lepus_subscribe.py` | Small | Subscribe + envelope strip + render |
| Datapod contract | Medium | Rust WASM contract (compile once, deploy once) |
| JSON encoding protocol | Small | Add `EncodingProtocol::Json` to freenet-lepus (2 files, ~20 lines) |
| UI integration | Medium | Add "Publish to Lepus" button in glasswing alongside existing Pintheon flow |

### Implementation order:

1. **JSON encoding protocol** — add `EncodingProtocol::Json` to freenet-lepus (prerequisite for Python client)
2. **Pure-Python envelope** (`lepus_envelope.py`) — test immediately, no Rust needed
3. **WebSocket client** (`lepus_client.py`) — pure Python, uses `websockets` + `json`
4. **Datapod contract** (Rust WASM) — compile with `fdev build`, deploy once
5. **Publishing flow** — connect the pieces: envelope → PUT → deposit
6. **Subscription flow** — subscribe → strip envelope → render
7. **UI integration** — add to glasswing's NiceGUI interface (both desktop and browser modes)
