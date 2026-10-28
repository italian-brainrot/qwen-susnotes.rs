# susnotes development tasks. Run `just` for the list, `just check` for what CI runs.
#
# The recipes here are the contract the CI workflow enforces, kept in one place so a
# local run and a GitHub run cannot disagree. `just env` says whether this machine has
# everything the workspace needs (the same check CI runs in its `tooling` job).

# what can I run?
default:
    @just --list

# the CI contract: format check, lints as errors, all the tests
check: fmt-check clippy test

# every binary, in debug, in target/debug/
build:
    cargo build --workspace

# the desktop app
run:
    cargo run -p susnotes-app

# the command line, e.g. `just cli --help` or `just cli stats --json`
cli *args:
    cargo run -q -p susnotes-cli -- {{ args }}

# the HTTP API end to end in one process: vault, server, client, assertions
smoke:
    cargo run -q -p susnotes-api --example smoke

# the vault round trip the CI `vault-compat` job runs, in a throwaway folder
vault-smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    # two notes at the end, and the link survived the rename, or the format changed
    # under us and this sequence will say so.
    scratch=$(mktemp -d -t susnotes-vault-smoke.XXXXXX)
    trap 'rm -rf "$scratch"' EXIT
    export SUSNOTS_STATE_DIR="$scratch/state" SUSNOTS_PASSWORD="just vault smoke"
    cargo build -q -p susnotes-cli
    BIN=target/debug/susnotes-cli
    $BIN init "$scratch/vault" --kdf test
    $BIN today "a journal line"
    $BIN write "A note" --content="links to [[Another note]]"
    $BIN verify
    $BIN read "A note" --raw | grep -q "Another note"
    $BIN rename "A note" "Renamed note"
    $BIN read "Renamed note" --raw | grep -q "Another note"
    $BIN --json stats | grep -q '"notes":2'
    $BIN --json search another | grep -q '"note":"Renamed note"'
    echo "vault format round trip ok"

# formatting only; `check` verifies it instead
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

# the lint set lives in the workspace Cargo.toml; warnings are errors on purpose
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# clippy's machine-editable lints. Only run it on a clean tree, and only when you

# intend to review what it writes.
fix:
    cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged

test:
    cargo test --workspace

# mutation testing, which proves the tests fail for the right reasons. Slow; the
# target is the core crate because that is where the vault and its crypto live, and

# mutants.out stays ignored (see .gitignore).
mutation:
    cargo mutants -p susnotes-core

# why is a dependency in the graph?
tree *args:
    cargo tree {{ args }}

# is this machine ready to build, test and run this workspace?
env:
    ./scripts/dev-env-check.sh

# the checks on the tasks and scripts themselves, run by CI in its `tooling` job
lint: lint-just lint-scripts

lint-just:
    just --unstable --fmt --check

lint-scripts:
    shellcheck --severity=warning scripts/*.sh

clean:
    cargo clean
