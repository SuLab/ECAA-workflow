# Emission-blocking conditions (vendored parity fixture, WS-D3)

The compiler ALWAYS emits a package. Only these four human-required
conditions prevent emission:

1. Missing or contradictory SME intent that cannot be classified into any modality
2. Deterministic schema-validation failure on a required intake field where no default exists
3. Explicit SME rejection at the confirmation gate (`reject` endpoint)
4. Explicit operator kill-switch (an emission-side analogue to ECAA_GIT_ENABLED=0)

Plus one deterministic, non-SME-facing defense-in-depth gate that is not a
human decision:

5. A task's container reference is not digest-pinned
   (`validate_container_digests_pinned`, the first statement of
   `emit_package`).
