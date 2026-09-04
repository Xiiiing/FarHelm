.PHONY: check test test-ui run-hub run-console smoke smoke-worker privacy release test-release

check:
	./scripts/check-version-policy.sh
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	corepack pnpm@10.17.1 --dir farhelm-console lint
	corepack pnpm@10.17.1 --dir farhelm-console typecheck

test:
	cargo test --workspace
	corepack pnpm@10.17.1 --dir farhelm-console test
	cd farhelm-worker-codex && uv run pytest

test-ui:
	corepack pnpm@10.17.1 --dir farhelm-console build
	corepack pnpm@10.17.1 --dir farhelm-console test:e2e

run-hub:
	test -n "$$FARHELM_HUB_CONFIG"
	cargo run -p farhelm-hub -- serve --config "$$FARHELM_HUB_CONFIG"

run-console:
	corepack pnpm@10.17.1 --dir farhelm-console dev

smoke:
	./tests/smoke.sh

smoke-worker:
	cargo run -p farhelm-agent -- worker-smoke

privacy:
	./scripts/check-private-files.sh
	./scripts/check-readme-parity.sh

release:
	./scripts/build-release.sh

test-release: release
	./tests/deployment-package.sh
