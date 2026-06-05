# Contributing to THALIOX

THALIOX is an operating system for AI, by AI — built in the open. We welcome contributors who want to
help build it, from a one-line fix to owning a whole subsystem. This guide covers **how to contribute via
a pull request** and **how to apply for developer access**.

> Read [README.md](README.md) and [RFC-0001 — the TAM Abstract Machine](docs/rfcs/0001-abstract-machine.md)
> first. Every change must respect the five invariants (INV-1…INV-5). The single most important review
> question is: *"Which validated hypothesis does this serve?"*

---

## 1. Ground rules

- **Top-down, staged.** Land the smallest change that delivers value. Don't build for stages we haven't reached yet.
- **Humans are the floor.** Anything that touches capabilities, budgets, or audit must keep the system auditable, reversible, and takeover-able.
- **Be kind, be precise.** Assume good faith, review the code not the person, and back claims with evidence (a test, a benchmark, a repro).

## 2. Set up

```bash
git clone https://github.com/thaliox/thaliox-os.git
cd thaliox-os
cargo build --workspace
```

Requirements: Rust **1.96+** (`rustup`). Cargo lives at `~/.cargo/bin`.

## 3. The four gates (must pass before review)

Every PR must be green on all four. Run them locally first:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

No `clippy` allows without justification in the PR. New behavior needs a test. Public items need rustdoc.

## 4. Pull request workflow

1. **Fork** the repo and create a branch off `main`: `git checkout -b feat/short-topic`.
2. **Commit** with [Conventional Commits](https://www.conventionalcommits.org/): `feat(runtime): …`, `fix(cap): …`, `docs: …`.
   Keep commits focused; explain the *why* in the body.
3. **Keep PRs small and single-purpose.** A reviewable PR is one idea. Split unrelated changes.
4. **Describe** the PR: what hypothesis it serves, which invariants it touches, how you verified it (paste test output).
5. **Open the PR** against `main`. CI runs the four gates; a maintainer reviews. Address feedback by pushing follow-up commits (we squash on merge).

By submitting a PR you agree to license your contribution under **Apache-2.0 OR MIT** (the repo's dual license).

## 5. Becoming a THALIOX developer

"Developer" is a recognized role with triage rights, review weight, and push access to feature branches.
The path is deliberately simple and merit-based — **you apply with a PR**:

1. **Land at least one merged PR** that passes all four gates. This is your proof of work.
2. **Open a "developer application" PR** that adds yourself to [`CONTRIBUTORS.md`](CONTRIBUTORS.md):
   - your name / handle and a contact,
   - the area you want to help own (e.g. `cognition`, `cap`, `fabric`, docs, web),
   - links to your merged PR(s).
   Use the commit message `chore(contributors): apply for developer access — <handle>`.
3. **A maintainer reviews** against two questions: *Is the work sound?* and *Does the area need an owner?*
   On approval we merge the PR and grant the `developer` role.

Want a bigger mandate (owning a crate, driving a milestone)? Say so in the application — call out the
subsystem and the milestone (see [MASTER_PLAN.md](docs/MASTER_PLAN.md)) you want to push.

## 6. Where to start

- Good first changes: docs, examples, test coverage, small `clippy`/ergonomics fixes.
- Bigger bites: a new `Tool`, an `LlmProvider` backend, capability-verifier hardening, gateway endpoints.
- Track progress and open questions at [thaliox.dev](https://thaliox.dev); read the docs at [thaliox.io](https://thaliox.io).

Questions? Open a GitHub Discussion or issue. Thank you for helping build an OS for AI. 🛰️
