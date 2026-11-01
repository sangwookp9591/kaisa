# Recorded CLI spec reports

## PASS

PASS (PROVISIONAL SPEC) (reduced: descendants)

- Pass: all attempt containment admitted — all recorded attempt containment shortfalls admitted (evidence: ledger:29, ledger:38, ledger:47, ledger:53, ledger:63, ledger:72)
- Pass: spec approval status — provisional spec; explicitly allowed by policy (evidence: frozen-spec.json:blake3:beb5bed0593d4b67aa47eb519d684826409f7227c275ce0188169808bddb0a5c)
- Pass: spec digest unchanged — approved specification and repository copies unchanged (evidence: spec-integrity-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.json:blake3:92a58073b0016ee00e33754bda9c8f6ca05b36c9a69b1e26073816609a8009c2)
- Pass: traceability complete — all requirements and criteria mapped to final-tree acceptance tests (evidence: traceability-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.json:blake3:988b90a88ca35b7cc73012b51a636a89af513806956359bb880c93bb51b171af)
- Pass: deterministic checks — trusted checks, protected assets and deterministic lint passed (evidence: deterministic-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.json:blake3:e5058915ff4f411b29195aabe4cb7e9a949b708f638429acfc1f4a9254b2de5d)
- Pass: private QA all passed on FINAL tree — acceptance + adversarial: 2/2 passed on final tree; qa_evidence: candidate-influenceable-in-process (evidence: qa-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.json:blake3:d6eb7f91efe340ed520950db61f8299fccc15b9dfe34a3c0f12510f437823428)
- Pass: domain test review no blocking — domain test review has no blocking finding (evidence: domain-test-review-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.json:blake3:0f1a4d2ed215998c2a438feb4ccfe02439b00e4b5bf4f4800b62c1d76e4d8797, test-review-transcript-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.log:blake3:f0d88375b72c901545404a4f8d8c0168ce641effdad8410fc49c2d66cc87d57c)
- Pass: staged review approved — PASS (reduced: descendants); staged review approved with no unresolved blocking finding; admitted on platform-limit (evidence: staged-review-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.json:blake3:956d7d59b9030de4497cd293f96d21a8b1b27007590ba8c3927b66523252d372, staged-review-context-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.json:blake3:fa2b6633248ebab8b0d17b0b91f922c1f21a1e913344d6c7dbed6a7f28f37564, review-transcript-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.log:blake3:2167f8f6f151ad669b443e1eca38baae05a80fff38369b70b7afb4fb92dc00df)
- Pass: characterization honoured — legacy characterization honoured on final tree (evidence: characterization-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.json:blake3:18255e38dfe39573d4dc97b90f6e1db78f4c1c8dc965f363c9f36d72a8890341)
- Pass: evidence preserved — ledger chain, private manifests and final-tree evidence artifacts preserved (evidence: preservation-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.json:blake3:6b6f6d94cb286216cff13dc29e88483197cb4c020297307cc6e11fa6a5a2c334)
- Pass: qa_isolation full or policy-accepted — qa_isolation: full; explicit acceptance: None (evidence: qa-16c6f87bcb32a8e867ae88a1d6673f6d5dec3382.json:blake3:d6eb7f91efe340ed520950db61f8299fccc15b9dfe34a3c0f12510f437823428)

# Traceability: run_dmjvasnqos740000czb0000000

Final tree: 16c6f87bcb32a8e867ae88a1d6673f6d5dec3382

| Requirement | Criterion | Private tests (final-tree result) | Development tests (lint-mapped) |
|---|---|---|---|
| R1 | AC-1 | empty-input [acceptance]: PASS (16c6f87bcb32a8e867ae88a1d6673f6d5dec3382); empty-input [adversarial]: PASS (16c6f87bcb32a8e867ae88a1d6673f6d5dec3382) |  |


Result: kaisa/result/run_dmjvasnqos740000czb0000000; retrieve with git merge --ff-only kaisa/result/run_dmjvasnqos740000czb0000000

## FAIL

FAIL (PROVISIONAL SPEC) (reduced: descendants)

- Pass: all attempt containment admitted — all recorded attempt containment shortfalls admitted (evidence: ledger:86, ledger:95, ledger:104, ledger:110, ledger:120, ledger:129)
- Pass: spec approval status — provisional spec; explicitly allowed by policy (evidence: frozen-spec.json:blake3:beb5bed0593d4b67aa47eb519d684826409f7227c275ce0188169808bddb0a5c)
- Pass: spec digest unchanged — approved specification and repository copies unchanged (evidence: spec-integrity-863a4fc6ae92984067f80285eb5de8faf69642b4.json:blake3:92a58073b0016ee00e33754bda9c8f6ca05b36c9a69b1e26073816609a8009c2)
- Pass: traceability complete — all requirements and criteria mapped to final-tree acceptance tests (evidence: traceability-863a4fc6ae92984067f80285eb5de8faf69642b4.json:blake3:3c27bacb5c6ebfed87695b1ff318af7f8b8b6e8937c911cd6f91d33bd54a3bc6)
- Pass: deterministic checks — trusted checks, protected assets and deterministic lint passed (evidence: deterministic-863a4fc6ae92984067f80285eb5de8faf69642b4.json:blake3:e5058915ff4f411b29195aabe4cb7e9a949b708f638429acfc1f4a9254b2de5d)
- Fail: private QA all passed on FINAL tree — acceptance + adversarial: 0/2 passed on final tree; qa_evidence: candidate-influenceable-in-process (evidence: qa-863a4fc6ae92984067f80285eb5de8faf69642b4.json:blake3:50781bd5429db0b9aaa9f02afbf8e18ad75462e7257b7f4c8f036aa7dda9d2c8)
- Pass: domain test review no blocking — domain test review has no blocking finding (evidence: domain-test-review-863a4fc6ae92984067f80285eb5de8faf69642b4.json:blake3:ee5c8ba39e05ce5eb0dc9df1b839d5cfaa37ed09687318765301a0385f3284c1, test-review-transcript-863a4fc6ae92984067f80285eb5de8faf69642b4.log:blake3:f0d88375b72c901545404a4f8d8c0168ce641effdad8410fc49c2d66cc87d57c)
- Pass: staged review approved — PASS (reduced: descendants); staged review approved with no unresolved blocking finding; admitted on platform-limit (evidence: staged-review-863a4fc6ae92984067f80285eb5de8faf69642b4.json:blake3:99faabda8489bed8558931b055f50311029d7dec216c3e0284e44e50924e254c, staged-review-context-863a4fc6ae92984067f80285eb5de8faf69642b4.json:blake3:53e215858a6421cd60122fc663a010ca999b83aa8814dd37005ee459fdd1adb2, review-transcript-863a4fc6ae92984067f80285eb5de8faf69642b4.log:blake3:5b9428750af395b33908eeed1521f5652d624e835aa798aa3eec5a8edac8f00f)
- Fail: characterization honoured — required correction AC-1 preserved current behaviour: legacy-empty (evidence: characterization-863a4fc6ae92984067f80285eb5de8faf69642b4.json:blake3:619a4b30de3bb1b4e1030f661f9341a91dc94fc556657871aecc791e5ff51cc2)
- Pass: evidence preserved — ledger chain, private manifests and final-tree evidence artifacts preserved (evidence: preservation-863a4fc6ae92984067f80285eb5de8faf69642b4.json:blake3:6b6f6d94cb286216cff13dc29e88483197cb4c020297307cc6e11fa6a5a2c334)
- Pass: qa_isolation full or policy-accepted — qa_isolation: full; explicit acceptance: None (evidence: qa-863a4fc6ae92984067f80285eb5de8faf69642b4.json:blake3:50781bd5429db0b9aaa9f02afbf8e18ad75462e7257b7f4c8f036aa7dda9d2c8)

# Traceability: run_dmjvav21lmu80000igt0000000

Final tree: 863a4fc6ae92984067f80285eb5de8faf69642b4

| Requirement | Criterion | Private tests (final-tree result) | Development tests (lint-mapped) |
|---|---|---|---|
| R1 | AC-1 | empty-input [acceptance]: FAIL/stale (863a4fc6ae92984067f80285eb5de8faf69642b4); empty-input [adversarial]: FAIL/stale (863a4fc6ae92984067f80285eb5de8faf69642b4) |  |

