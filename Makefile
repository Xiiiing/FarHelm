.PHONY: check test test-ui run-hub run-console smoke-worker privacy

check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	corepack pnpm@10.17.1 --dir farhelm-console lint
	corepack pnpm@10.17.1 --dir farhelm-console typecheck

test:
	cargo test --workspace
	corepack pnpm@10.17.1 --dir farhelm-console test
	cd farhelm-worker-codex && uv run pytest

test-ui:
	corepack pnpm@10.17.1 --dir farhelm-console test:e2e

run-hub:
	cargo run -p farhelm-hub

run-console:
	corepack pnpm@10.17.1 --dir farhelm-console dev

smoke-worker:
	cargo run -p farhelm-agent -- worker-smoke

privacy:
	./scripts/check-private-files.sh
	./scripts/check-readme-parity.sh
