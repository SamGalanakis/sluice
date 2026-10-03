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
| `lash.decide` | posts a decision note to the owner on the supplied thread, returns its message id, and requires no acknowledgement. An owner reply can override it. |

These copies use the standalone `sluice_fn` helper and protocol-1 envelope. The runtime
pins `_lashlib` with each bundle. Workers compose `agent.run` in their parent run and read
validated submissions through `ctx.submission()`. Lash permits one continuation for a typed
`WallCap` with a session. Both workers retain three retries with a 600-second backoff.

`lash.land` holds the `land` lease for fetch/rebase/push, releases it before checking and
reacquires it on the next iteration. An intentional refusal raises `Rejected` and registers
`ctx.retry_on_failure` against the explicit `work_step`, or the matching `-work` step for a
`-land` launch. Direct calls can supply `work_step`; without one they fail without a retry
action. Push failures retain their ordinary failure behavior because an unknown external
outcome must not retry the worker.
