# Claude Code — repository guidance

Read README.md, CONTRIBUTING.md, SECURITY.md and docs/building.md. Inspect the
working tree before editing and preserve unrelated work.

- Project-owned guest runtime and tooling is Rust. Inline assembly only at
  documented CPU/hardware boundaries. Document every unsafe invariant.
- Do not substitute a Linux kernel or a hosted UI for the guest OS.
- Run `cargo xtask ci` before finishing. Report actual results and untested
  limits; never claim boot or VM compatibility without real evidence.

Optional private maintainer notes may exist in `.agent-private/` (start with
`.agent-private/STATE.md`). They are not in public clones and are not needed
for ordinary contributions; if absent, work from the issue or request.

Never copy private notes, prompts or queries into commits, issues, pull
requests, logs or releases. Never force-add ignored paths. Check
`cargo xtask check-private` and staged files before any commit. Do not push or
publish without explicit maintainer authorization. This file grants no extra
access.
