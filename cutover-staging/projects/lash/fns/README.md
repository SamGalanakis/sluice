# lash project fns

Functions only the sluice `lash` project sees (its `fns/` dir). Local for now; not published.

| Fn | Does |
|---|---|
| `lash.fork` / `lash.fork_rm` | `kiln fork lash <name>` / `kiln rm lash <name>` (refuses a dirty fork unless `force`) |
| `lash.worker` | `agent.run` (Devin, Opus, or Codex) under the current lane header; `lands: false` by default means commit in the fork and let the plan call `lash.land`; `lands: true` means prove with focused tests and one `kiln clippy`, then rebase and push to main. The worker writes a summary of at most 120 words to its run dir, returned as `summary` and `final`; open steps submit other declared outputs with `step_submit`. A submitted `head_sha` is checked against the fork |
| `lash.land` | Squashes with `title` when given (a completing change ends its title with ` (<ticket>)`, a partial one has body `Part of <ticket>`), then rebases onto origin/main and pushes. No gate. Fails at once on `ready=false` (never skips, so `after <unit>-landed` keeps waiting); amends a cargo-fmt pre-push refusal once and pushes again. |
| `lash.dev_test` | `. ./env.sh && python3 scripts/dev-test.py`; `{ok, code, tail}`, never fails on a red |
| `lash.on_main` | waits for a sha, or a commit matching `grep`, to be on origin/main |
| `lash.main_red` | waits for the next completed `ci.yml` dispatch run on main by `databaseId` (overridable `workflow` and `event`); returns `{red, failed_jobs, failed_job_ids, failed_tests, sha, url, run_id}` |
| `linear.create` / `linear.comment` / `linear.close` | the `linear` CLI; `close` completes the issue on `Closes <ticket>` in the body or a title ending ` (<ticket>)`, else comments only |
| `lash.decide` | logs a non-blocking orchestrator decision: appends it numbered to REVISIT.md and posts an inbox item with Accept/Override (nothing waits; an Override answer wakes the orchestrator via `/workspace/notes/lash/watch.py`) |
