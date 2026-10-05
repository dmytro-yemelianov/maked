# maked — make + ed: Yemelianov (Emelyanov) Dmytro

Formerly *makeyd* (v0.1.x) and, before that, *rmake*.

A zero-dependency POSIX make (IEEE Std 1003.1) with GNU extensions, written in
Rust, plus an executable Lean 4 model of make's freshness semantics.

- `rust_make/`: the `maked` binary. It has a parallel DAG executor, a GNU
  jobserver (client and master), SHA-256 content hashing (`--hash`), a
  content-addressable cache (`--cache`), Ninja import and export
  (`-f build.ninja`, `--emit-ninja`), `--emit-compdb`, Chrome trace output
  (`--trace`), a live TUI (`--tui`) and remote workers.
- `lean_make/`: the Lean 4 model, with theorems about the model (graph, -jN scheduling bounds,
  freshness, cache key). They are kernel-checked statements about the Lean
  model and **not** a proof about the Rust binary.
- `benchmarks/fuzzer/`: a differential fuzzer that runs maked, GNU make and
  the Lean model on random DAGs. Agreement there is test evidence, not proof.

For the architecture, what the Lean model does and does not prove, and benchmarks
against GNU make and Ninja, see **[Inside maked](docs/inside-maked.md)**.

## Install

Download an archive for your platform from
[Releases](https://github.com/dmytro-yemelianov/maked/releases), check it
against `SHA256SUMS`, and put `maked` on your `PATH`. Each archive also
contains the man page (`man/maked.1`) and bash/zsh/fish completions.

| Platform | Archive |
| --- | --- |
| Linux x86_64 (static, musl) | `maked-<tag>-x86_64-unknown-linux-musl.tar.gz` |
| Linux aarch64 (static, musl) | `maked-<tag>-aarch64-unknown-linux-musl.tar.gz` |
| macOS Apple Silicon | `maked-<tag>-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `maked-<tag>-x86_64-apple-darwin.tar.gz` |
| macOS universal | `maked-<tag>-universal2-apple-darwin.tar.gz` |
| Windows x86_64 | `maked-<tag>-x86_64-pc-windows-gnu.zip` |

macOS binaries are cross-built and not notarized. After extracting, run
`xattr -d com.apple.quarantine maked`.

On Windows, recipes run through `$SHELL` if it is set and `%COMSPEC%`
(cmd.exe) otherwise. The jobserver and the remote-worker shell are Unix-only.

### ESP32 / microcontrollers

There is no ESP32 build, and one would not be useful. The crate does compile
for `xtensa-esp32-espidf` except for `AtomicU64`, which xtensa lacks. The
blocker is ESP-IDF's `std::process::Command`: it is a stub that returns
`Unsupported`, and make's job is spawning recipe shells. There is no `fork`,
`exec` or `/bin/sh` to run them. A `no_std` parser and graph library would be
a separate product.

## Build from source

```sh
make            # cargo build --release + lake build
make test       # cargo test, lake build, differential fuzzer
sudo make install PREFIX=/usr/local
```

This needs Rust 1.88 or newer (edition 2024; CI uses 1.88.0). The Lean model and the fuzzer need
elan (Lean `v4.30.0` per `lean_make/lean-toolchain`), Python 3 and GNU make.

## Compatibility

`benchmarks/realworld/run.py` builds **zlib, sqlite, redis, git, jq and Lua**
with maked and with GNU make and compares five things: the build, a smoke
test of the program, the files produced, a null build and the rebuild after
touching a header. All six match. CI runs the suite on every push to `main`
(`.github/workflows/realworld.yml`). `rust_make/tests/gnu_compat_tests.rs`
holds the minimal cases those builds turned up, each checked against GNU make
itself.

Known differences from GNU make:

- Double-colon rules (`t::`) are merged: all their prerequisites first,
  then the recipes in order. GNU make runs each as a separate rule.
- An order-only prerequisite (`t: a | dir`) that is missing is built like a
  normal one. One that exists is ignored.
- `$(flavor)` guesses: a value containing `$` is reported as recursive.
- `$(eval)` during the build applies only variable assignments. New rules
  can only come from `$(eval)` while the makefile is read.
- `-k` (keep going) is accepted but stops at the first error.
- A backslash-newline in a recipe is joined into one line before reaching
  the shell.

## Files maked writes

- `.maked_log` holds recipe durations from earlier builds. The parallel
  scheduler uses them to start the longest remaining path first. Deleting
  it only loses that ordering hint. Add it to `.gitignore`.
- `.maked.db` is written only with `--hash`, and `.maked_cache/` only with
  `--cache`.

## Remote workers: security

A worker runs the recipe commands a coordinator sends it, so since v0.1.3
it only accepts coordinators that hold a shared token:

```sh
# on both machines (file mode 0600; the token is never passed on argv)
head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > ~/.maked-worker-token
export MAKED_WORKER_TOKEN_FILE=~/.maked-worker-token

maked --worker-listen=127.0.0.1:7070                    # worker, loopback only
maked --worker-listen=0.0.0.0:7070 --worker-allow-remote # worker for other hosts
maked -j8 --remote-workers=build1:7070,build2:7070       # coordinator
```

- Each request and response is signed with HMAC-SHA256 over a fresh nonce
  from the worker. A wrong or missing token gets nothing executed, and a
  captured request cannot be replayed.
- The worker listens on loopback unless `--worker-allow-remote` is given.
- Input and target paths must be relative and free of `..`. Each request
  gets its own sandbox, and the coordinator accepts only the target file
  back.
- Messages are capped at 256 MiB, and a client has 60 s to send its
  request.

Traffic is authenticated but **not encrypted**: sources and outputs cross
the network in clear text. Between hosts, use an SSH tunnel or a VPN. If a
worker is unreachable or fails, the target is built locally.

## CI and releases

Workflows run on the shared self-hosted `raps-ci` box
(`runs-on: [self-hosted, raps-ci, maked]`). All six release targets are
cross-compiled there by `scripts/ci/package.sh`. Pushing a `v*` tag builds the
targets and publishes a GitHub Release with `SHA256SUMS`. To provision a
runner, use `scripts/ci/provision-raps-ci-runner.sh`.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option. `benchmarks/lua_test/` holds Lua 5.4.9 (MIT, © Lua.org,
PUC-Rio), which is used only as a benchmark workload.
