EXE    := wreckless
TARGET := $(shell rustc --print host-tuple)

RUSTFLAGS ?= -C target-cpu=native
export RUSTFLAGS

ifdef MSYSTEM
	NAME := $(EXE).exe
	ENV  := UNIX
else ifeq ($(OS),Windows_NT)
	NAME := $(EXE).exe
	ENV  := WINDOWS
else
	NAME := $(EXE)
	ENV  := UNIX
endif

PGO_DIR := target/pgo-profiles

ifeq ($(ENV),UNIX)
	PGO_MOVE := mv "target/$(TARGET)/release/$(EXE)" "$(NAME)"
	PGO_BIN  := ./target/$(TARGET)/release/$(EXE)
	PGO_RUN   = LLVM_PROFILE_FILE="$(PGO_DIR)/wreckless_%m_%p.profraw" $(PGO_BIN)
else
	PGO_MOVE := move /Y "target\$(TARGET)\release\$(EXE).exe" "$(NAME)"
	PGO_BIN  := target\$(TARGET)\release\$(EXE).exe
	PGO_RUN   = set "LLVM_PROFILE_FILE=$(PGO_DIR)\wreckless_%%m_%%p.profraw" && $(PGO_BIN)
endif

.PHONY: all no-syzygy pgo bolt wasm x64-check checkdeps clean help

all: ## Build the engine
	cargo rustc --release --bin wreckless -- --emit link=$(NAME)

no-syzygy: ## Build without syzygy support
	cargo rustc --release --bin wreckless --no-default-features -- --emit link=$(NAME)

# The instrumented binary is driven directly rather than through `cargo pgo
# run`, which clears the profile directory on each invocation -- successive runs
# through it would discard each other instead of accumulating.
#
# Three depths rather than one. A profile taken only at the default depth leaves
# the shallow pruning paths (LMP, futility, razoring) and the deep
# singular/extension paths thinly covered, and those are most of what a real
# game executes. The larger hash on the last run exercises TT replacement
# against a fuller table.
pgo: ## Build with profile-guided optimization
	cargo pgo instrument
	$(PGO_RUN) bench 128 1 8
	$(PGO_RUN) bench 128 1 12
	$(PGO_RUN) bench 256 1 14
	cargo pgo optimize
	$(PGO_MOVE)

# BOLT is a separate pass from PGO and composes with it: PGO informs codegen,
# BOLT re-lays out the linked binary from a second profile. Needs llvm-bolt and
# perf2bolt on PATH -- `make checkdeps` reports whether they are present.
bolt: ## Build with PGO, then BOLT (post-link block/function layout)
	cargo pgo instrument
	$(PGO_RUN) bench 128 1 8
	$(PGO_RUN) bench 128 1 12
	$(PGO_RUN) bench 256 1 14
	cargo pgo optimize
	cargo pgo bolt build --with-pgo
	cargo pgo bolt optimize --with-pgo
	$(PGO_MOVE)

wasm: ## Build the WebAssembly target
	RUSTFLAGS= rustup run nightly \
		cargo build -Z build-std=panic_abort,std \
		--lib --target wasm32-unknown-unknown --release --no-default-features
	wasm-bindgen target/wasm32-unknown-unknown/release/wreckless.wasm --target web --out-dir pkg
	wasm-opt -O3 --enable-simd --enable-threads --enable-relaxed-simd \
		pkg/wreckless_bg.wasm -o pkg/wreckless_bg.wasm

x64-check: ## Check compilation for x86-64 v1-v4
	RUSTFLAGS="-C target-cpu=x86-64" cargo check --target x86_64-unknown-linux-gnu --no-default-features
	RUSTFLAGS="-C target-cpu=x86-64-v2" cargo check --target x86_64-unknown-linux-gnu --no-default-features
	RUSTFLAGS="-C target-cpu=x86-64-v3" cargo check --target x86_64-unknown-linux-gnu --no-default-features
	RUSTFLAGS="-C target-cpu=x86-64-v4 -C target-feature=+gfni,+avx512bw,+avx512vl,+avx512vbmi,+avx512vbmi2,+avx512vnni,+avx512bitalg" cargo check --target x86_64-unknown-linux-gnu --no-default-features

checkdeps: ## Verify build dependencies are installed
	@echo "-- required --"
	@command -v rustc >/dev/null 2>&1 && echo "  rustc        ok" || (echo "  rustc        MISSING"; exit 1)
	@command -v clang >/dev/null 2>&1 && echo "  clang        ok" || echo "  clang        MISSING (required for Syzygy; use 'make no-syzygy' to skip)"
	@echo "-- pgo --"
	@command -v cargo-pgo >/dev/null 2>&1 && echo "  cargo-pgo    ok" || echo "  cargo-pgo    missing (run: cargo install cargo-pgo)"
	@echo "-- bolt (optional, for 'make bolt') --"
	@command -v llvm-bolt >/dev/null 2>&1 && echo "  llvm-bolt    ok" || echo "  llvm-bolt    missing (install LLVM with BOLT)"
	@command -v perf2bolt >/dev/null 2>&1 && echo "  perf2bolt    ok" || echo "  perf2bolt    missing (install LLVM with BOLT)"
	@echo "-- wasm --"
	@rustup toolchain list 2>/dev/null | grep -q nightly && echo "  nightly      ok" || echo "  nightly      missing (run: rustup toolchain install nightly)"
	@command -v wasm-bindgen >/dev/null 2>&1 && echo "  wasm-bindgen ok" || echo "  wasm-bindgen missing (run: cargo install wasm-bindgen-cli)"
	@command -v wasm-opt     >/dev/null 2>&1 && echo "  wasm-opt     ok" || echo "  wasm-opt     missing (install binaryen)"

clean: ## Remove build artifacts
	cargo clean
	rm -f "$(EXE)" "$(EXE).exe"

help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"} /^[a-zA-Z0-9_-]+:.*?##/ { \
		printf "  %-12s %s\n", $$1, $$2 \
	}' $(MAKEFILE_LIST)
