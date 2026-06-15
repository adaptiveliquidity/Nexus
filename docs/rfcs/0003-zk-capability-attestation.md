# RFC 0003 — Zero-Knowledge Capability Attestation

- **Status:** Draft (research / design only — no production code)
- **Roadmap:** P3, Research (most speculative / long-horizon of the three RFCs)
- **Author:** Nexus

## 1. Summary

Investigate letting an agent prove it holds a valid capability token **without
revealing the token itself** — a zero-knowledge attestation of the statement
"I hold a non-expired token, signed in a valid attenuation chain, that grants
capability C." This is privacy-preserving delegation across trust boundaries.

**Headline finding:** technically achievable, but the cost/benefit is poor for
the current single-trust-domain design. The dominant cost is **proving an Ed25519
signature inside a ZK circuit**, which is expensive. This RFC recommends **not**
building it now, documents what it would take, and identifies the one realistic
near-term motivator (cross-trust-domain delegation).

## 2. Context — the capability model today

From `src/security/capability.rs`:

```text
Capability = ReadFile(PathBuf) | WriteFile(PathBuf) | ListDirectory(PathBuf)
           | HttpGet(String) | HttpPost(String) | ExecuteBinary(PathBuf)
           | MountTmpfs(PathBuf) | All | None

CapabilityToken {
    id: Uuid,
    capability: Capability,
    granted_by: String,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    parent_id: Option<Uuid>,   // attenuation lineage
    chain_depth: u32,          // 0 = root; capped at DEFAULT_MAX_CHAIN_DEPTH = 5
    signature: Vec<u8>,        // Ed25519 over the serialized token
}
```

Verification today (`CapabilityManager::authorize` / chain walk):
1. Ed25519 `verify_signature` against the manager's `VerifyingKey`.
2. Expiry + revocation checks.
3. Walk the attenuation chain to the root, verifying each ancestor's signature,
   monotonic depth, and the `is_subset_of` narrowing relation.

Crucially, **the verifier already sees the full token** (capability, paths,
lineage). The ZK question is: can the prover convince the verifier of the *result*
of steps 1–3 while hiding the token contents (paths, ids, lineage)?

## 3. What we would prove in zero knowledge

Statement: *"I know a token T (and its ancestor chain) such that:*
- *each link's Ed25519 signature verifies under the known issuer public key(s);*
- *`now < expires_at` for every link;*
- *each child `is_subset_of` its parent;*
- *the leaf capability `allows` the concrete request R (a public input);*
- *`chain_depth <= 5`*  — *without revealing T, the paths, the ids, or the chain."*

Public inputs: issuer public key(s), current time (or an epoch), and the concrete
requested capability R (or a commitment to it). Private witness: the token chain
and signatures.

## 4. Scheme comparison

| Scheme | Proof size | Verify time | Prover cost | Trusted setup | Notes for this use case |
|--------|-----------|-------------|-------------|---------------|-------------------------|
| **Groth16** | ~200 B | ~1–2 ms | High | **Per-circuit** (toxic waste) | Smallest proofs/fastest verify, but per-circuit setup is operationally painful as `Capability` evolves |
| **PLONK** (universal SRS) | ~400 B–1 KB | a few ms | High | Universal (one-time) | Better ops story than Groth16; circuit can change without new ceremony |
| **Halo2** (no trusted setup) | ~a few KB | ~ms–tens of ms | High | **None** | Best trust story; mature Rust (`halo2`); larger proofs |
| **Bulletproofs** | ~1–2 KB (log) | **slow** (linear) | Medium | None | Verify too slow for a hot authorize path; better for range proofs than circuit-SNARKs |
| **STARKs** | ~tens–hundreds KB | fast-ish | High | None (hash-based, PQ) | Large proofs; post-quantum; overkill here |

Baseline to beat: current Ed25519 verify is on the order of **tens of
microseconds**. **Every ZK option is 1–3 orders of magnitude slower to verify**
and far slower to prove. ZK does not win on performance — it only wins on
*privacy*.

## 5. The real cost driver: Ed25519-in-circuit

The expensive part is not the capability logic (string/path subset checks and
small comparisons are cheap-ish in-circuit) — it is **verifying Ed25519 signatures
inside the circuit**, repeated for every link in the attenuation chain (up to 6
signatures for a depth-5 chain). Ed25519 over Curve25519 is not SNARK-friendly;
in-circuit EdDSA verification is a well-known heavyweight gadget.

Mitigations, in rough order of leverage:

1. **Swap the signature primitive to a SNARK-friendly one** for the attestation
   path: EdDSA over a JubJub-style embedded curve, or Poseidon-based signatures.
   This is the standard move (e.g. Zcash/Sapling), but it means the capability
   system would issue ZK-friendly tokens *in addition to* the Ed25519 tokens —
   a significant dual-stack.
2. **Recursive proofs / accumulation** (Halo2/Nova-style) to fold the per-link
   signature checks, amortizing chain depth.
3. **Prove once, reuse** within a short epoch so the prover cost is not paid per
   request.

## 6. Threat model — what the verifier learns

- **Without ZK (today):** verifier learns the full capability, exact paths/URLs,
  token ids, and the entire delegation lineage.
- **With ZK (this proposal):** verifier learns only that *some* valid token grants
  the *specific requested* capability R — not the broader grant, not the paths
  beyond R, not the ids, not who delegated to whom.
- **Residual leakage to design against:** the requested capability R is public (it
  has to be, to enforce it), so ZK hides the *token*, not the *action*. Replay and
  proof-malleability must be prevented: bind each proof to a fresh
  challenge/nonce + epoch (public input) so a captured proof cannot be reused, and
  prefer non-malleable proof systems. Revocation is harder in ZK (you can't check
  a revealed id against a revocation list) — needs an accumulator / nullifier
  scheme, adding more circuit cost.

## 7. Recommendation

**Do not implement now.** Justification:

- The current deployment is a **single trust domain** where the verifier is the
  same authority that issued the tokens — it already knows everything, so hiding
  the token from it yields little.
- The performance cost is steep (ms-scale verify vs µs-scale today) on what is a
  hot authorization path.
- It would require a parallel ZK-friendly signature stack alongside Ed25519.

**Revisit when** a concrete cross-trust-domain requirement appears — e.g. an agent
must prove authorization to a *third party* that should not see the full grant or
the delegation graph. That is the only setting where the privacy benefit justifies
the cost.

If/when revisited, the recommended starting point is **Halo2 (no trusted setup) +
a JubJub-EdDSA token variant**, proven once per short epoch and bound to a nonce.

## 8. If we ever build it — phased sketch

1. **Spike:** model the circuit cost of one in-circuit EdDSA verification in
   `halo2` (or `arkworks`) to get real prover/verify numbers on target hardware.
2. **Token variant:** define a ZK-friendly `AttestableToken` (JubJub-EdDSA,
   Poseidon hashing) issued alongside Ed25519 tokens.
3. **Circuit:** signature-chain verification + `is_subset_of` + expiry/epoch +
   `allows(R)` + depth bound, with a nonce binding.
4. **Revocation:** nullifier/accumulator design so revoked tokens can't prove.
5. **Integration:** an `attestation` verification path parallel to
   `CapabilityManager::authorize`, opt-in per deployment.

## 9. Candidate Rust crates

- **`halo2` / `halo2_proofs`** — no trusted setup, mature, active. Recommended
  default if built.
- **`arkworks`** (`ark-groth16`, `ark-plonk`, `ark-ec`, `ark-ed-on-bls12-381` for
  embedded EdDSA) — flexible toolkit if Groth16/PLONK is preferred.
- **`bellman`** — classic Groth16 (Zcash lineage); narrower but battle-tested.
- **`curve25519-dalek` / `ed25519-dalek`** — current (non-ZK) primitives; retained
  for the standard path regardless.

## 10. Open questions

- Is there *any* near-term third-party verifier in the roadmap, or is this purely
  speculative? (Determines whether §8 ever starts.)
- Could a far cheaper non-ZK mechanism (e.g. issuing a narrowly-scoped short-lived
  token via the existing `attenuate()` so the third party only ever sees the
  minimal grant) satisfy the actual privacy requirement without ZK? **Very likely
  yes** — attenuation already produces minimal-disclosure tokens, and this should
  be the first thing tried before any ZK work.

## 11. References

- Ed25519 / EdDSA: RFC 8032.
- Groth16: "On the Size of Pairing-based Non-interactive Arguments" (Groth, 2016).
- PLONK: Gabizon, Williamson, Ciobotaru, 2019.
- Halo2 book: <https://zcash.github.io/halo2/>
- Bulletproofs: Bünz et al., 2018.
- arkworks: <https://github.com/arkworks-rs>
- In-repo capability model: `src/security/capability.rs` (Ed25519 tokens,
  `attenuate()`, `is_subset_of`, `DEFAULT_MAX_CHAIN_DEPTH`).
