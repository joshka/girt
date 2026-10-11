# Delta reuse integrity probes (Astra)

These are original storage-engine regression tests, run on 2026-10-11 against the nine candidate
heads recorded in `results.json`. They deliberately assert the desired invariant; known failures are
evidence, not an expectation encoded into the tests. Do not merge this probe branch as a passing
implementation.

## Reproduction

```sh
export PATH="$HOME/.cargo/bin:$PATH"
unset RUSTUP_TOOLCHAIN
cargo test --test probe_astra -- --nocapture
cargo test --lib astra_unsynced_index -- --nocapture
```

The integration file is unchanged across all candidates. Copy it onto a new change based on the
desired candidate. The retirement fault-model test is included inside the existing
`retention::retire::tests` module, so its fixture and existing private checkpoint are available.
`results.json` records each candidate commit and the observed assertions. Native host: macOS arm64;
toolchain and Git versions recorded there. Tests create disposable directories only.

## Fixture provenance and assertions

All fixture logic is original. Git initializes repositories, hashes blobs, creates tree objects,
creates pack indexes, and verifies the valid source packs. A small original encoder produces
four-byte whole blobs and explicit OFS delta chains; `git index-pack` and `git verify-pack`
independently accept them. No upstream Git implementation or test source was consulted.

The damaged-entry cases modify an identity in a Git-written index, re-sort records, rebuild fanout,
and recompute the index checksum. Pack data and CRCs remain unchanged. This models a repository that
already contains a mislabelled entry; it does not claim such repositories pass Git's full integrity
check. The assertions distinguish publication of unreadable replacement content from deletion of the
last independently readable source copy.

- `source_base_identity_does_not_change_delta_content`: one pack supplies the correct base; another
  contains a mislabelled base and a valid OFS-derived target. Require target bytes in every newly
  indexed replacement and in the live repository, or refusal before publication.
- `primary_loose_precedence_survives_alternate_copy`: require primary loose bytes to survive copying
  when an alternate has a damaged entry with that identity.
- `joined_depth_is_checked_before_publication`: valid source chains of depth two and one join at a
  shared object. Policy depth is two. Require the new indexed pack to remain readable by a cold
  reader under the same policy. Each read opens fresh storage; no warmed cache masks it.
- `retirement_preserves_the_only_copy_of_an_omitted_object`: an old pack contains one required
  object and one omitted object. Assert the omitted object's only copy remains readable. This
  encodes the strict all-object preservation instruction; ordinary unreachable-object GC is a
  different policy and is not intrinsically corruption.
- `keep_pack_content_is_not_duplicated` and `recent_pack_content_is_not_duplicated`: three
  maintenance runs with one new loose root per run, counting readable copies of protected content
  and total pack bytes. These are storage amplification probes, not timing claims.
- `recent_extra_keeps_its_last_readable_copy`: a recent, otherwise unrequired delta has a
  mislabelled source base. Assert at least one independently readable pack copy survives.
- `damaged_protected_entry_does_not_justify_removing_readable_copy`: a damaged `.keep` entry must
  not justify retiring the only readable copy in an ordinary pack.
- `damaged_first_blob_does_not_remove_the_readable_copy`: a tree points at a blob with a damaged
  first pack entry and a readable later pack entry. Assert maintenance preserves a readable copy.
- `reused_entries_obey_logical_input_budget`: two four-byte packed blobs plus one loose byte, with
  an eight-byte write budget. Refusal preserves the old logical payload accounting. Acceptance
  establishes changed accounting, not a process-memory or data-loss claim.

## Crash model and limits

`astra_unsynced_index_reappearance_does_not_break_cold_open` uses the existing retirement
unlink/sync checkpoints. It models an unsynchronized index unlink being lost while the later pack
unlink persists. A directory sync commits the pending unlink in this model. The assertion is that
fresh object storage still opens. A separate fixture assertion requires the old pack actually to be
retired, preventing a vacuous pass when the replacement is identical.

This confirms cold-open failure under the stated persistence model and demonstrates the barrier
control. It does not establish that APFS produces that order in a physical power loss. Actual
filesystem persistence, publication ordering, MIDX persistence and concurrent scan/copy replacement
remain open; they must not be presented as demonstrated physical data loss.

The matrix tests one hash format and small deterministic inputs. Fable's independent Git-shaped
chain probe additionally exercises the default depth of 64 with source depths 51 and 46. Performance
evidence belongs to the shared benchmark and Fable's isolated dedupe-port measurement; Astra did not
duplicate those measurements.
