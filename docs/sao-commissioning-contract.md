# SAO ↔ OrionII commissioning contract

OrionII v0.2 replaces the silent `GET /api/orion/birth` handshake with an interactive **commissioning** flow. This document is the contract for the sibling SAO repo. Implement these endpoints; OrionII's `src-tauri/src/orion/commissioning_client.rs` is the only client.

## Vault distinction

SAO Vault stores every Ed25519 keypair behind a typed reference. Two key types:

```jsonc
// MentorKey
{
  "kind": "mentor",
  "mentor_id": "<uuid>",
  "public_key": "<base64-32-bytes>",
  "created_at": "<rfc3339>"
}

// EntityKey
{
  "kind": "entity",
  "agent_id": "<uuid>",
  "mentor_id": "<uuid>",
  "public_key": "<base64-32-bytes>",
  "created_at": "<rfc3339>"
}
```

Private halves never leave the vault. SAO signs on behalf of the holder via vault sign operations. The cockpit and OrionII never see private bytes. `kind` is the discriminator both for vault-side authorization (a mentor sign request must produce a mentor signature, never an entity one) and for the SAO admin UI.

## Endpoints

All endpoints are bearer-authenticated with the `agent_token` from the bundle's `config.json`. All bodies and responses are JSON, `camelCase` on the wire.

### `POST /api/orion/commission/start`

Begins a commissioning session. SAO mints (or re-uses) the entity keypair and returns the public-key fingerprints the cockpit displays at the Identity stage.

**Request**

```json
{
  "clientVersion": "0.2.0"
}
```

**Response 200**

```json
{
  "commissionId": "<uuid>",
  "mentorId": "<uuid>",
  "agentId": "<uuid>",
  "mentorPublicKeyFpr": "<sha256-fingerprint-hex>",
  "entityPublicKeyFpr": "<sha256-fingerprint-hex>",
  "allowedRoleKeys": [
    "calendar_assistant",
    "tech_document_reader_writer",
    "email_triage",
    "research_analyst",
    "project_coordinator",
    "compliance_reviewer"
  ],
  "qAndAEnabled": true,
  "saoProvider": "anthropic",
  "saoIdModel": "claude-haiku-4-5-20251001",
  "saoEgoModel": "claude-haiku-4-5-20251001"
}
```

**Errors**
- `401 Unauthorized` — token invalid or expired. OrionII routes to the **Repair → Refresh credentials** sub-mode.
- `409 Conflict` — agent already commissioned and the bundle is being re-used. Body includes `{ "code": "already_commissioned", "agentId": "..." }`. OrionII routes to **Repair → Re-bind to existing agent**.
- `429 Too Many Requests` — rate limit; standard `Retry-After` header.

### `POST /api/orion/commission/finalize`

Submits the operator-approved charter. SAO persists it, has the vault sign with both the mentor and entity keys, builds the birth certificate, and returns it. After this OrionII writes `charter.md` + `birth_certificate.json` and starts `OrionCore`.

**Request**

```json
{
  "commissionId": "<uuid>",
  "roleKey": "calendar_assistant",
  "charterText": "# Charter\n\n...full markdown...\n",
  "charterHash": "<hex-blake3-of-charterText-bytes>"
}
```

`charterHash` is `hex(blake3(charterText.as_bytes()))`. SAO MUST recompute it from `charterText` and reject the call if it disagrees — the field is a guard against stripping whitespace or invisible chars in transit.

**Response 200**

```json
{
  "agentId": "<uuid>",
  "orionId": "<uuid>",
  "soulRef": "blake3:<hex>",
  "charterHash": "<hex>",
  "birthCertificate": {
    "agentId": "<uuid>",
    "mentorPublicKey": "<base64-32-bytes>",
    "entityPublicKey": "<base64-32-bytes>",
    "charterHash": "<hex>",
    "roleKey": "calendar_assistant",
    "issuedAt": "<rfc3339>",
    "mentorSignature": "<base64-64-bytes>",
    "entitySignature": "<base64-64-bytes>",
    "saoSignature": "<base64-64-bytes>"
  },
  "defaults": {
    "provider": "anthropic",
    "idModel": "claude-haiku-4-5-20251001",
    "egoModel": "claude-haiku-4-5-20251001",
    "policyVersion": 1
  }
}
```

`soulRef` MUST equal `"blake3:" + charterHash`. OrionII verifies the equality and refuses to persist a mismatched certificate.

**Errors**
- `400 Bad Request` — body shape, hash mismatch, role not in the agent's allowlist.
- `401 Unauthorized` — same as start.
- `409 Conflict` — `commissionId` already finalized (idempotency). Body returns the existing certificate.

### `POST /api/orion/commission/repair`

Single endpoint with two modes via the `kind` field. Returns a refreshed `BirthCertificate` (same shape as finalize) plus the canonical `charterText` so OrionII can rebuild local state.

**Request — token rotation**

```json
{
  "kind": "rotate_token",
  "newToken": "<jwt>"
}
```

Use when the operator has a fresh `agent_token` (e.g. SAO admin reissued one). Mentor and entity keys remain unchanged; only the bearer used for HTTP rotates.

**Request — re-bind**

```json
{
  "kind": "rebind"
}
```

Use when local state was lost (laptop reimage) but the bundle's `agent_token` is still valid. SAO re-emits the existing certificate and the canonical `charterText`. Since SAO is the source of truth there is no recovery-bundle decryption step on the client.

**Response 200**

```json
{
  "agentId": "<uuid>",
  "orionId": "<uuid>",
  "soulRef": "blake3:<hex>",
  "charterHash": "<hex>",
  "charterText": "# Charter\n\n...",
  "birthCertificate": { /* as in finalize */ },
  "defaults": { /* as in finalize */ }
}
```

**Errors**
- `401 Unauthorized` — token invalid even after rotation. UI prompts the operator to download a fresh bundle.
- `404 Not Found` — `agentId` no longer exists in SAO (operator deleted the agent). UI tells the operator to create a new agent.

### `GET /api/orion/birth`

Idempotent re-read. Already exists; extend the response with `birthCertificate` and `charterHash`. OrionII calls this on every boot to detect drift between local `charter.md` and SAO's record. If they disagree, OrionII routes to **Repair → Re-bind**.

**Response 200**

```json
{
  "agent": { /* existing fields */ },
  "owner": { /* existing fields */ },
  "policy": { /* existing fields */ },
  "birthCertificate": { /* same shape as finalize */ },
  "charterHash": "<hex>",
  "charterText": "<canonical>"
}
```

## SAO admin UI requirements

Per agent, the admin UI MUST expose:

- Mentor and entity public-key fingerprints (read-only).
- Charter text (read-only render of the canonical bytes).
- Charter version history with timestamps and the SAO operator that approved each amendment (commission, charter.update).
- **Revoke** action that invalidates the bearer token and the entity key. OrionII detects revocation via 401 on next boot and routes to repair.
- **Rotate token** action that issues a new bearer; operator copies the JWT into the cockpit's Repair stage.

## Q&A LLM proxy

Commissioning's Q&A path uses the existing `POST /api/llm/generate` endpoint with a new role string `"commissioning"` (in addition to the existing `"id"` and `"ego"`). SAO MAY route this to a different provider/budget; OrionII does not depend on it. The conversation is short (≤10 turns) and ends when the LLM emits the literal end-token `[CHARTER_READY]` on its own line — OrionII strips the token and treats the immediately preceding turns as the charter source.

## Versioning

The contract version is `2`. Add `X-Orion-Commission-Version: 2` to every request. SAO returns `400 Bad Request` with `{ "code": "unsupported_commission_version", "expected": [...] }` on a major-version mismatch. Minor compatible additions (new optional fields) do not bump the version.

## Out-of-scope for v1

- Multi-mentor (one mentor per OrionII install).
- Charter co-signing by additional reviewers.
- Encrypted charter (the charter is not secret; only its signing keys are).
- Bundle-less / deep-link enrollment — keep the `Install-OrionII.cmd` path; deep links land in a future ADR.
