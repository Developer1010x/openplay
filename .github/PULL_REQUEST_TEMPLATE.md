## What this changes

<!-- One or two sentences. If it fixes an issue, write "Fixes #N". -->

## Why

<!-- The reasoning, especially if the change is not obviously correct. -->

## How it was verified

<!--
Be specific and honest. "Ran cargo test --all" beats "tested locally", and
"could not test on Windows, no machine" is a fine answer — an unstated gap
is not.
-->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --all`
- [ ] Tested against real hardware (say which device, or N/A)

## Anything reviewers should know

<!--
Platform-gated code? Only openplay-pipeline, -sender and -receiver escape the
cross-platform CI job, so say if a break there would go unnoticed.

Touching crypto? Confirm any constant is verifiable against a specification —
see docs/crypto.md.
-->
