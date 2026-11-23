# unemu-host

Host session libraries shared by a core's own program and the
[unemu front end](https://github.com/unemu-org/unemu). This repository owns
presentation and host input/output over
[unemu-core-api](https://github.com/unemu-org/unemu-core-api), without
depending on an emulator or the front end. It contains no egui and no
platform-specific emulation code.

- `machine-view`: a machine session over the contract, with host pacing,
  audio through cpal, winit keys, pads and pointers, screen geometry,
  resets and saved state. Its session is the only wall-clock reader, for
  pacing and slice deadlines; host time never enters the machine. An
  opt-in macOS system-key tap uses objc2. Tests run without a window or
  audio device; `test-support` exposes helpers for consumers' tests.
- `unemu-test-core` (`crates/test-core`): a scripted core that implements
  the whole contract and emulates nothing. Consumers use it to test host
  sessions through the contract, without linking an emulator.

The crates were imported verbatim from
[unemu at 2379e8190441a29c9e8f96700a89d00f778ceaf2](https://github.com/unemu-org/unemu/tree/2379e8190441a29c9e8f96700a89d00f778ceaf2/crates).
The original design is frozen in its
[architecture](https://github.com/unemu-org/unemu/blob/2379e8190441a29c9e8f96700a89d00f778ceaf2/backlog/docs/architecture.md)
and [core contract design](https://github.com/unemu-org/unemu/blob/2379e8190441a29c9e8f96700a89d00f778ceaf2/backlog/docs/design/core-contract.md).
[The repository decision](docs/decisions/0001-repository.md) records the boundary.

## Depending on the crates

[unemupc](https://github.com/unemu-org/unemupc)'s planned minimal viewer and
the unemu front end pin this repository by **exact git revision**, never a
branch or movable tag. Replace the placeholder with the full reviewed
40-hex commit when adding or moving a pin:

```toml
[dependencies]
machine-view = { version = "0.0.4", git = "https://github.com/unemu-org/unemu-host", rev = "<full 40-hex commit>" }

[dev-dependencies]
machine-view = { version = "0.0.4", git = "https://github.com/unemu-org/unemu-host", rev = "<same full 40-hex commit>", features = ["test-support"] }
unemu-test-core = { version = "0.0.4", git = "https://github.com/unemu-org/unemu-host", rev = "<same full 40-hex commit>" }
```

Consumers and these libraries must pin the same revision of
`unemu-core-api`, so Cargo uses one set of contract types. Neither this
repository nor the contract depends on a consumer; there is no repository
cycle. The contract crate stays dependency-free.

For local development, put overrides in the **consumer's** gitignored
`.cargo/config.toml` (and add that path to its `.gitignore`):

```toml
[patch."https://github.com/unemu-org/unemu-host"]
machine-view = { path = "../unemu-host/crates/machine-view" }
unemu-test-core = { path = "../unemu-host/crates/test-core" }
```

Paths are relative to the consumer's workspace root; adjust them to the
checkout layout. This repository also ignores `.cargo/config.toml`, so
local contract work can use:

```toml
[patch."https://github.com/unemu-org/unemu-core-api"]
unemu-core-api = { path = "../unemu-core-api" }
```

Remove local overrides before final checks and restore any patch-induced
lockfile changes. Never commit a local path override or a patched lockfile.
The layer guard deliberately rejects a locally patched contract until the
override is removed.

## Development

```sh
nix develop
cargo build --locked --workspace --all-targets
cargo fmt --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
bash scripts/layer-guard.sh --self-test
bash scripts/layer-guard.sh
bash scripts/publication-guard.sh --self-test
bash scripts/publication-guard.sh
cargo deny --all-features check
```

The devShell uses the same locked Rust toolchain as unemu, plus cargo-deny,
git and jq. Linux adds pkg-config and ALSA headers for cpal. The imported
flake inputs and `flake.lock` are retained unchanged; the unused Backlog.md
input installs nothing in the shell. No core, fixture or recipe tools are
needed. Without Nix, use Rust 1.99.1, a C compiler, git and jq; on Ubuntu
install `pkg-config` and `libasound2-dev` too.

CI runs on Ubuntu 22.04 and macOS, with read-only repository access.
`layer-guard.sh` checks every direct dependency, including development,
build, optional and target-specific dependencies, using
`cargo metadata --no-deps`. Host crates' transitive dependencies are
checked by cargo-deny against `deny.toml` and `Cargo.lock`.

## Licence

[AGPL-3.0-or-later](LICENSE).
