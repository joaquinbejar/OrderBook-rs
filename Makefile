# Makefile for common tasks in a Rust project
# Detect current branch
CURRENT_BRANCH := $(shell git rev-parse --abbrev-ref HEAD)
ZIP_NAME = OrderBook-rs.zip


# Default target
.PHONY: all
all: test fmt lint build

# Build the project
.PHONY: build
build:
	cargo build

.PHONY: release
release:
	cargo build --release

# Run tests
.PHONY: test
test:
	LOGLEVEL=WARN cargo test

# Format the code
.PHONY: fmt
fmt:
	cargo +stable fmt --all

# Check formatting
.PHONY: fmt-check
fmt-check:
	cargo +stable fmt --check

# Run Clippy for linting, plus the Production Panic Policy syntax gate
# (issue #242, ported from PriceLevel's issue #173; absolute since #260):
# clippy's `[lints.clippy]` restriction lints (Cargo.toml) and this crate's
# `clippy.toml` cover unwrap/expect/panic/unreachable/todo/unimplemented/
# indexing/string-slicing/narrowing-casts/raw-arithmetic in production.
# `lint-panic` below covers what clippy has NO lint for at all (the
# `assert!`/`debug_assert!` family), what clippy's own `#[cfg(test)]`
# heuristic can wrongly exempt (a standalone `#[cfg(test)]` production
# helper that is not a `mod tests { ... }` block), `saturating_*`/
# `wrapping_*` on production state, and any production
# `#[allow]`/`#[expect]` of a denied clippy lint (the escape hatch a plain
# `cargo clippy` cannot see, since the attribute suppresses the lint).
.PHONY: lint
lint: lint-panic
	cargo clippy --all-targets --all-features -- -D warnings

# Production Panic Policy syntax gate (issue #242): scripts/check_panic_policy.py.
# Runs the scanner's own fixture self-test first (a broken scanner must
# never silently report a clean src/), then scans src/ for real. Zero
# tolerance (issue #260): there is no allowlist ledger; any finding fails.
# The only exception form is the inline `panic-policy-allow-saturating`
# marker on a reviewed `saturating_*`/`wrapping_*` expression.
.PHONY: lint-panic
lint-panic:
	python3 scripts/check_panic_policy.py --self-test
	python3 scripts/check_panic_policy.py

.PHONY: lint-fix
lint-fix:
	# `-A clippy::manual_saturating_arithmetic`: `cargo clippy --fix` can
	# rewrite checked arithmetic into `saturating_*` (issue #242) — this
	# crate never wants that rewrite auto-applied. `lint-panic` re-scans
	# src/ for saturating_*/wrapping_* afterward as an independent,
	# non-autofix-dependent check.
	cargo clippy --fix --all-targets --all-features --allow-dirty --allow-staged \
		-- -D warnings -A clippy::manual_saturating_arithmetic
	$(MAKE) lint-panic

# Clean the project
.PHONY: clean
clean:
	cargo clean

# Pre-push checks
.PHONY: check
check: test fmt-check lint

# Run the project
.PHONY: run
run:
	cargo run

.PHONY: fix
fix:
	cargo fix --allow-staged --allow-dirty

.PHONY: pre-push
# Ordering (issue #242, ported from PriceLevel's issue #173 review):
# `lint-fix` runs BEFORE `fmt`, not after. `cargo clippy --fix` can rewrite
# code (including, before `-A clippy::manual_saturating_arithmetic` above,
# into `saturating_*`) without reformatting it, so running `fmt` first and
# `lint-fix` last used to leave the tree unformatted after a "clean"
# pre-push. `lint-panic` runs after `fmt` since it is a plain text scan
# unaffected by formatting, and before `test` so a Production Panic Policy
# regression fails fast.
pre-push: fix lint-fix fmt lint-panic test readme doc

.PHONY: doc
doc:
	cargo clippy -- -W missing-docs

.PHONY: doc-open
doc-open:
	cargo doc --open

.PHONY: publish
publish: readme
	find . -name ".DS_Store" -type f -delete | true
	cargo login ${CARGO_REGISTRY_TOKEN}
	cargo package
	cargo publish

.PHONY: coverage
coverage:
	export LOGLEVEL=WARN
	cargo install cargo-tarpaulin --locked --version '>=0.37.5'
	mkdir -p coverage
	cargo tarpaulin --exclude-files 'benches/**' --all-features --workspace --timeout 120 --out Xml

.PHONY: coverage-html
coverage-html:
	export LOGLEVEL=WARN
	cargo install cargo-tarpaulin --locked --version '>=0.37.5'
	mkdir -p coverage
	cargo tarpaulin --exclude-files 'benches/**' --verbose --all-features --workspace --timeout 120 --out Html --output-dir coverage

.PHONY: coverage-json
coverage-json:
	export LOGLEVEL=WARN
	cargo install cargo-tarpaulin --locked --version '>=0.37.5'
	mkdir -p coverage
	cargo tarpaulin --exclude-files 'benches/**' --verbose --all-features --workspace --timeout 120 --out Json --output-dir coverage

.PHONY: open-coverage
open-coverage:
	open coverage/tarpaulin-report.html

# Rule to show git log
git-log:
	@if [ "$(CURRENT_BRANCH)" = "HEAD" ]; then \
		echo "You are in a detached HEAD state. Please check out a branch."; \
		exit 1; \
	fi; \
	echo "Showing git log for branch $(CURRENT_BRANCH) against main:"; \
	git log main..$(CURRENT_BRANCH) --pretty=full

.PHONY: create-doc
create-doc:
	cargo doc --no-deps --document-private-items

.PHONY: readme
readme: check-cargo-readme create-doc
	cargo readme > README.md

.PHONY: check-cargo-readme
check-cargo-readme:
	@command -v cargo-readme > /dev/null || (echo "Installing cargo-readme..."; cargo install cargo-readme)

.PHONY: check-spanish
check-spanish:
	cd scripts && python3 spanish.py ../src && cd ..

.PHONY: zip
zip:
	@echo "Creating $(ZIP_NAME) without any 'target' directories, 'Cargo.lock', and hidden files..."
	@find . -type f \
		! -path "*/target/*" \
		! -path "./.*" \
		! -name "Cargo.lock" \
		! -name ".*" \
		| zip -@ $(ZIP_NAME)
	@echo "$(ZIP_NAME) created successfully."


.PHONY: check-cargo-criterion
check-cargo-criterion:
	@command -v cargo-criterion > /dev/null || (echo "Installing cargo-criterion..."; cargo install cargo-criterion)

.PHONY: bench
bench: check-cargo-criterion
	cargo criterion --output-format=quiet

.PHONY: bench-show
bench-show:
	open target/criterion/reports/index.html

.PHONY: bench-save
bench-save: check-cargo-criterion
	cargo criterion --output-format quiet --history-id v0.4.8 --history-description "Version 0.3.2 baseline"

.PHONY: bench-compare
bench-compare: check-cargo-criterion
	cargo criterion --output-format verbose

.PHONY: bench-json
bench-json: check-cargo-criterion
	cargo criterion --message-format json

.PHONY: bench-clean
bench-clean:
	rm -rf target/criterion

.PHONY: bench-compare-refs
# Cross-version A/B comparison harness (issue #258/#259): builds the
# small standalone `benches/compare/` crate against two git refs in two
# detached worktrees (default: the `v0.13.1` tag vs `HEAD`) and reports
# a summary table. Pass through extra flags after `--`, e.g.:
#   make bench-compare-refs ARGS="--quick --rounds 1"
#   make bench-compare-refs ARGS="--baseline v0.13.1 --candidate HEAD --rounds 5"
# See `scripts/bench_compare.sh --help` and BENCH.md "Methodology".
bench-compare-refs:
	./scripts/bench_compare.sh $(ARGS)

.PHONY: bench-hdr
bench-hdr:
	cargo bench --bench add_only_hdr
	cargo bench --bench add_only_risk_hdr
	cargo bench --bench cancel_only_hdr
	cargo bench --bench cancel_single_owner_hdr
	cargo bench --bench fill_single_owner_hdr
	cargo bench --bench aggressive_walk_hdr
	cargo bench --bench notional_walk_hdr
	cargo bench --bench mixed_70_20_10_hdr
	cargo bench --bench thin_book_sweep_hdr
	cargo bench --bench mass_cancel_burst_hdr
	cargo bench --bench mass_cancel_by_user_hdr
	cargo bench --bench stp_sweep_hdr
	cargo bench --bench stp_contention_hdr
	cargo bench --bench reserve_sweep_hdr
	cargo bench --features special_orders --bench pending_stops_hdr


.PHONY: workflow-coverage
workflow-coverage:
	DOCKER_HOST="$${DOCKER_HOST}" act push --job code_coverage_report \
       -P ubuntu-latest=catthehacker/ubuntu:latest \
       --privileged

.PHONY: workflow-build
workflow-build:
	DOCKER_HOST="$${DOCKER_HOST}" act push --job build \
       -P ubuntu-latest=catthehacker/ubuntu:latest

.PHONY: workflow-lint
workflow-lint:
	DOCKER_HOST="$${DOCKER_HOST}" act push --job lint

.PHONY: workflow-test
workflow-test:
	DOCKER_HOST="$${DOCKER_HOST}" act push --job run_tests

.PHONY: workflow
workflow: workflow-build workflow-lint workflow-test workflow-coverage

.PHONY: tree
tree:
	tree -I 'target|.idea|.run|.DS_Store|Cargo.lock|*.md|*.toml|*.zip|*.html|*.xml|*.json|*.txt|*.sh|*.yml|*.yaml|*.gitignore|*.gitattributes|*.gitmodules|*.git|*.gitkeep|*.gitlab-ci.yml' -a -L 3

# Build every example binary with all feature flags and run each in
# turn, recording per-example pass / fail / timeout. JSON results land
# at smoke-results.json; per-example logs at /tmp/orderbook-smoke-*.log.
.PHONY: smoke-test
smoke-test:
	./scripts/smoke-test.sh
