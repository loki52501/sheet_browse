T-28 acceptance fixture (crates/harness/tests/accept_t28.rs and bench/results/accept-T-28.md).

browseros.jsonl: nine lines in the shape scripts/hooks/browseros_ledger.py writes. `__CANARY__`
and `__RUN__` are placeholders the test replaces at run time (`__CANARY__` with
SB_VAULT_CANARY, planted as vault entry `pw`), so no vault value is ever in this file (rule 2).

Lines: 1 navigate compose, 2 snapshot (refs e7, e9 named), 3 act fill e7 with the To address,
4 act fill e9 with the canary, 5 act click e30 (no name in any summary: needs a model target),
6 wait text, 7 evaluate (not a recipe tool), 8 act failed, 9 navigate with the canary in the URL
(@vault outside act text).
