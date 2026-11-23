# AGENTS.md — unemu-host

- Talk to the maintainer in Polish. Everything committed is English: code,
  comments, documentation, identifiers, branches and commits.
- This is a public AGPL-3.0-or-later repository. Keep all content safe to
  publish: never name private hosts, private paths or personal setups, and
  never commit proprietary software, firmware, manuals or screenshots.
  Run `bash scripts/publication-guard.sh` before review.
- Commit only on an explicit request. Workers leave changes uncommitted
  for the supervisor to review.
- This repository has no beads tracker yet. Its work is tracked in
  [unemupc](https://github.com/unemu-org/unemupc) under the prefix `upc`.
- Keep the dependency boundary in `scripts/layer-guard.sh`: the contract,
  the necessary host crates and the local scripted core for session tests.
  No egui, emulator dependencies or platform-specific emulation belongs here.
- Host time belongs to session pacing and never enters a core's execution.
  Do not launch GUI windows or take screenshots during automated checks.

Use `nix develop` for the pinned toolchain. Run formatting, Clippy, workspace
tests with all features, both guards and their self-tests. Follow a
supervisor's environment, wrapper and sandbox instructions when supplied.
