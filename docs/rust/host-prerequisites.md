# Rust execution host prerequisites

Execution requires Linux, cgroup v2 at `/sys/fs/cgroup`, a reachable systemd
user manager, delegated user services, `pidfd_open` and a readable
`/proc/sys/kernel/random/boot_id`. `HostCheck::run` returns a serializable
report with one actionable result for each prerequisite. Any failed result
blocks execution. `sluice doctor` prints the report (`--json` for the full
object, with the engine profiles and the selected release's manifest check).

The delegation check creates a unique `sluice-test-doctor-*.service` through
argument-safe `systemd-run --user` argv with `Delegate=yes` and
`KillMode=control-group`. It opens `cgroup.subtree_control` for writing,
creates a control child, moves its own probe process there, enables `pids`,
creates a payload child and checks inherited controllers and writable child
control. It stops its service even on failure or cancellation. A degraded
manager can still pass if it supports these operations. No live services are
changed.

## Build the private artifact

Run from the worktree:

```sh
scripts/build-private-tmux
# Or choose a release-local output prefix:
scripts/build-private-tmux /path/to/release/private-tmux
```

The default prefix is `target/private-tmux`, outside PATH. The build refuses
system directories, PATH directories and the owner's live home. It downloads
[tmux 3.7c](https://github.com/tmux/tmux/releases/tag/3.7c) and
[libevent 2.1.12-stable](https://github.com/libevent/libevent/releases/tag/release-2.1.12-stable)
from their official release assets and verifies committed SHA-256 digests.
The tmux digest was independently computed on 2026-10-03 and matches GitHub's
release asset metadata. The libevent digest was computed from its official
asset on the same date; the older GitHub asset has no digest metadata.

Libevent builds out of tree into `<prefix>/libevent` with
`--disable-shared --enable-static --disable-openssl`. Tmux links its static
`libevent_core.a`, with `--disable-systemd --disable-cgroups`. These switch
names match the release's `configure.ac`. The shipped generated `cmd-parse.c`
avoids a yacc/bison dependency. Configure receives `YACC=true` for its tool
presence check; make receives `YACC=false` so an attempted regeneration fails.
Ncurses/tinfo remain dynamic system dependencies.

Build tools are a C compiler, make, curl, tar, pkg-config, Python 3 and
coreutils. Debian/Ubuntu package names are `build-essential`, `curl`, `tar`,
`pkg-config`, `python3`, `coreutils` and `libncurses-dev`. The script reports
missing packages and exits without installing anything or prompting. It never
sets Cargo environment variables. Both native builds and their temporary
sources stay under the output prefix. Installed tmux and libevent licenses
remain in that prefix; generated config is `share/tmux/config.defs`.

`tmux-manifest.json` records both releases, source URLs/digests, configure
flags, generated preprocessor definitions, binary SHA-256, dynamic ncurses version, parser
provenance, build host and UTC date. This release generates no config.h.
Its configuration is the Makefile's `DEFS` line, which must contain neither
`HAVE_SYSTEMD` nor `ENABLE_CGROUPS`. The script and loader check that line,
the binary digest, exact `tmux -V` result, static libevent, absence of
libsystemd and resolved dynamic tinfo/ncurses libraries.
It requires `/usr/bin/sha256sum` and `/usr/bin/ldd` for these bounded probes.
The manifest is release provenance, not a signature against a malicious
user who can rewrite both the binary and manifest.

`ApprovedTmux` invokes a canonical absolute binary path. Server argv always
includes `-D -S tmux.sock -f /dev/null`, or an explicit config file inside the private run directory. Clients use the
same private socket, and control clients add `-C attach-session`. Commands
run in a caller-owned mode-0700 directory, clear `TMUX` and `TMUX_PANE`, and
never address the default server. Load an immutable release prefix; replacing
its binary after approval is outside the supported release lifecycle.

## Delayed containment gate

After building the artifact:

```sh
cargo --config 'build.target-dir="target"' test -p sluice-process -- --ignored containment --nocapture
```

The tests require `/usr/bin/systemctl`, `/usr/bin/systemd-run`,
`/usr/bin/timeout`, `/usr/bin/sleep` and `/usr/bin/setsid`. All unit names begin
`sluice-test-`. Every service has a cleanup guard and a runtime limit.
`SLUICE_PRIVATE_TMUX_PREFIX` can select another private release prefix;
`SLUICE_CONTAINMENT_EVIDENCE` can select a report directory. Neither affects
the installed live home.

Each service runs a private foreground tmux with a pane shell, an ordinary
sleep and a sleep detached through setsid. The fixture records all five
persistent processes, including the service worker and server, with PID,
/proc start time, session ID and v2 cgroup path immediately after readiness
and again after at least 3.2 seconds. It verifies the detached child has a
separate session. The private gate requires all identities inside the service
subtree at both observations and no live identities after unit stop.

The host gate deliberately invokes only `/usr/bin/tmux 3.4` with the same
private socket fixture, expects the delayed pane to escape and records any
identities that survive stopping the service. It then kills only its own
recorded processes through pidfds. A host tmux that no longer migrates fails
this red-side gate, requiring fresh evidence. No existing sessions receive
keys or signals. JSON evidence goes under `target/p3-00-evidence` by default.

This proves host prerequisites and the pinned tmux's containment.

## Process ownership API

`TransientService::for_run(RunId)` reserves `sluice-run-<UUID>.service`, the
name every production run unit uses. `for_test` uses `sluice-test-<UUID>.service`
and is for tests only. Both start through argument-safe
`systemd-run --user` with `Delegate=yes`, `KillMode=control-group`,
`Restart=no`, and a description. `start_once` reserves its in-memory attempt
before the first await and reconciles every ambiguous command result by that
same unit name. `StartOutcome::Uncertain` never permits another launch.
The coordinator must persist its spawn-attempted claim before calling this
API and use `adopt` when reconstructing a production handle. Unit queries,
stop and reset use bounded `systemctl` commands.

Open the reconciled service cgroup while it exists and retain the `Cgroup`
handle. Operations resolve against its pinned directory descriptor, so a
removed service can be proven empty without reopening a replacement at the
same path. `RunCgroups::create` creates `control`, moves the guardian there,
and enables delegated `pids` before creating `payload`. `invocation` creates
an exclusive `payload/<InvocationId>` leaf; duplicate IDs fail.
The caller must own the service it supplies. Sluice service name validation
prevents using an unrelated root, but is not an admission capability.

The executable routes its internal `payload-exec` mode to
`launcher::payload_exec_main(args, dispatch)` before executing any fn. Its
arguments are a fresh nonce, a barrier timeout in milliseconds, and dispatcher
arguments. `PreparedLaunch::spawn` uses the same executable with no initial
arguments, preserves its command environment/cwd/stdout/stderr, and installs
an inherited socketpair as stdin. There is no pre-exec stop or callback.
The post-exec launcher announces nonce and process identity and blocks.
`continue_in` verifies placement, calls the guardian's durable recording
closure, rechecks cancellation and grants continuation. The launcher verifies
the placed identity, replaces stdin with `/dev/null`, acknowledges
`RunStarted`, closes the barrier descriptor and invokes the injected dispatcher.
The dispatcher can exec uv or run a Rust fn; it must explicitly configure
payload input if needed. No payload inherits the barrier socket.

Spawn and barrier calls are blocking and belong on a dedicated thread or
`spawn_blocking`; the guardian must serialize grant delivery with cancellation
state transitions. A recording failure or pre-grant cancellation kills and
reaps the launcher without dispatch. A grant/acknowledgement failure can have
an unknown dispatch outcome: the API kills the leaf, and the guardian must
still prove cleanup before releasing holds. It must never retry that executor
launch. `RunningPayload` owns the child wait and original pidfd; its handle is
not an invocation cleanup proof.

`ProcessIdentity` contains PID, procfs start time, boot ID and cgroup path.
`OwnedProcess` checks the recorded generation on both sides of opening a
pidfd. It signals only that descriptor, and pidfd polling distinguishes exited
processes, including zombies, from live ones. Procfs descendant and CPU
snapshots include waited-child counters and exclude roots/zombies. Detached
or reparented work must also be observed through cgroup membership.

After closing admission, `stop_invocation` sends TERM through pidfds, waits
five seconds by default, escalates through recursive `cgroup.kill`, and
returns `EmptyProof` only after `cgroup.events` reports `populated 0`.
`StopPolicy` permits shorter test grace periods. `stop_run`, called outside
the service, reconciles its cgroup, stops/reset-fails the unit and proves the
pinned service subtree empty. `wait_owned_gone` also proves every recorded
pidfd exited. Errors and timeouts retain resource holds.

`FileLock::try_acquire` holds a flock descriptor and records project/run plus
PID/start-time/boot metadata. Conflicts return the holder after matching its
metadata to the kernel's `/proc/locks` flock owner and live identity. A short
flock on the adjacent `<lock>.publish` file serializes acquisition/publication
with conflict reads, including successive acquisitions by the same process.
Initialization races retry for one second; unverifiable metadata returns an
error rather than naming a stale holder. Both lock files must remain on the
same inode and must never be unlinked. These Linux primitives use the workspace's pinned Rust
1.97.0, rustix 1.1.5 with the local `stdio` feature, procfs 0.18.0 and fs4 1.1.0.

The ignored launch host tests extend the artifact gates with the actual
post-exec fixture mode, cancellation/record-failure injection, exclusive
invocation leaves, TERM-resistant double-fork work, private tmux server/pane,
setsid children, a mock app-server child, and guardian SIGKILL. They observe
all identities for at least 3.2 seconds and require recursive emptiness and
pidfd exit after cleanup. Every disposable service has a stop/reset drop
guard. Build the fixture with the workspace/all-target test recipe before
running these gates. JSON evidence is under `target/p3-01-evidence`.
