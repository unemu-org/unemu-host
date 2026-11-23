# Shared host session repository

Date: 2026-11-23
Status: accepted by the maintainer

## Context

Both a core's own program and the unemu front end need the same host session
layer. unemupc will get a minimal viewer binary built on `machine-view`.
The contract repository, `unemu-core-api`, must keep its contract crate
dependency-free. Neither consumer may introduce a repository dependency cycle.

## Decision

Keep the shared host layer in the public `unemu-org/unemu-host` repository,
licensed AGPL-3.0-or-later. Start with a verbatim import of
`crates/machine-view` and `crates/test-core` from unemu at
`2379e8190441a29c9e8f96700a89d00f778ceaf2`.

`machine-view` owns the host session over the contract: pacing and slice
deadlines (the only wall-clock reads), host audio through cpal, winit keys,
pads and pointers, screen geometry, and an opt-in macOS system-key tap
through objc2. `unemu-test-core` implements the whole contract as a scripted
test core and emulates nothing.

unemupc's viewer and unemu pin these libraries by exact git revision. The
libraries depend only on `unemu-core-api`, the host crates they need and
their transitive dependencies. Session tests may use the local scripted
core. No dependency points back to a core or unemu. No egui or
platform-specific emulation code belongs here.

## Rejected alternatives

- Put the host layer in the contract repository. This would mix the small,
  dependency-free contract with host libraries and their release concerns.
- Leave the host layer in unemu. A core's own viewer would then depend on
  the front end that depends on the core, introducing a repository cycle,
  or would have to duplicate the host layer.

## Consequences

Both consumers share one host layer without a cycle. They must coordinate
the exact contract revision so all contract types come from one Cargo
dependency. Host-layer changes are reviewed here before consumers move
their pins. Local checkouts use gitignored Cargo patches; committed
manifests retain exact git revisions.
