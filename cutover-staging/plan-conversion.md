# Plan conversion for the cutover importer

These staging files do not edit or install into the live home. The importer owns the
one-time plan conversion. Re-authoring the six recipes by intent is a separate operation
from carrying over existing plans.

## Conversion rules

1. Preserve step ids, declaration types, bindings, paused flags, resource needs, existing
   tags, outputs and gate order. Move `when: R` into the step's `after` list as the boolean
   entry `R`. Remove `when`. Deduplicate identical entries, retaining their first position.
2. Carry each existing `after` entry over as a plain entry. Do not add `?` based on status,
   a cleanup-looking name, or the rewritten recipes. Under v2 a plain entry requires success.
   Recipe cleanup intent applies only when expanding a newly authored recipe.
3. Keep every existing `unit:` tag. Do not join units across gates or data handoffs. For an
   untagged multi-step group, consider only connections between its untagged members and tag
   all members `unit:<first-step-id>`, taking first from the stored step order. Untagged
   singletons remain units of one. Refuse a generated name collision or multiple unit tags
   for owner resolution; do not silently regroup already tagged units.
4. Preserve scalar handoffs and list-valued `in.<name>.source` arrays verbatim, including
   their order. A cross-unit handoff never changes membership. Retain references from plan
   outputs too. Unit aggregation is ordinary data flow.
5. Existing plans have no delivery `exit` tags. Preserve that absence, so v2 derives their
   exits from intra-unit sinks. Do not apply a recipe's new delivery tags retroactively.
   New recipe expansion tags delivery explicitly. Any owner decision to alter a carried
   unit's exits is a separate plan edit.
6. No thread/inbox fn appears in either observed live plan or any of these six recipes.
   If a fresh snapshot introduces one, rewrite `thread.post` / `inbox.ask` to `message.post`
   and `thread.wait` to `message.wait`, preserving whether it waits and whom it addresses.
   Do not carry old inbox ids, log-derived threads or REVISIT records into new state.

The importer must compile the complete converted plan against the final v2 registry before
installation. A missing fn, stale input shape, invalid unit name or broken reference is a
conversion conflict. These notes are not authorization to remove or coerce invalid work.

## Read-only live observations

Read with `sluice tool plan_get` on 2026-10-03 at about 20:39 UTC. These are snapshots; the
owner's home stays active, and the importer must recount its own drained snapshot.

| Case | Lash, revision 2134 | Figments, revision 41 |
|---|---:|---:|
| Steps | 552 | 65 |
| Tagged units | 108 | 15 |
| Untagged steps / multi-step groups | 0 / 0 | 0 / 0 |
| `when` entries to move | 238 | 5 |
| Existing `after` entries to retain as plain | 355 | 22 |
| Existing `?` entries | 0 | 0 |
| Scalar and array handoff references | 617 | 55 |
| Handoff references crossing unit tags | 25 | 0 |
| `after` references crossing unit tags | 197 | 1 |
| Existing `exit` tags | 0 | 0 |
| Old message fn steps | 0 | 0 |

The plan JSON responses had SHA-256 digests
`80de77d4bf2b36b25e6167aa3918228c378264b4e42dbdb6e43c2ddd0b17c622` for Lash and
`65b550a4abdb4bcc2bceb435d0b5b654b65f4660447262dc644e6405882aefbe` for Figments.
The raw responses stay in temporary files, not in this repository.

## Cases that matter

- Lash has 76 six-step units and 32 three-step units. Its 238 conditions comprise 198
  `land/landed` refs and 40 `work/ready` refs. Those 40 legacy land conditions carry over
  as boolean gates. They can skip land when false, unlike the newly authored lane recipe,
  which always invokes land after a valid work handoff and rejects `ready=false`. The
  importer must not silently replace these old conditions with new recipe behavior.
- `ta-integrate-work.in.lanes.source` contains 25 lane `final` references, each from a
  different unit. Retain this entire array. The fixture `audit-fan-in.json` preserves
  the actual source ids and proves the model keeps 26 separate units with all 25 handoffs.
- Lash's 197 cross-unit ordering entries include prerequisites on `-landed`, the
  `ta-history-work` prerequisite of audit workers and the `ta-integrate-work` prerequisite
  of their cleanup. Preserve the step gates; do not rewrite them as unit dependencies.
- Figments' `teardown` unit has 35 steps, `lashbump-integrate` has 7, two units have 4,
  two have 3, and nine are singletons. Five runbook steps are gated by
  `teardown-deploy/ready`. Preserve that boolean condition and the long unit membership.
- Figments' sole cross-unit ordering entry is
  `runbook-refresh-live.after = ["teardown-rb-gaps"]`. It must remain an ordering entry
  between the two existing units.
- No live fn/recipe callback migration is needed beyond `lash.decide`. Its new output
  is `id: int`, a message sequence id, replacing the inbox item id and per-file number.
  Neither live plan contains a `lash.decide` step or consumer of its old outputs.
