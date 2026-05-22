BINARY := target/release/laramux
LOCAL_BIN ?= $(HOME)/.local/bin

.PHONY: build install clean release

build:
	@cargo build --release
	@echo "Binary at $(BINARY)"

install: build
	@install -Dm755 $(BINARY) $(LOCAL_BIN)/laramux
	@echo "Installed to $(LOCAL_BIN)/laramux"

clean:
	@cargo clean

release:
ifndef VERSION
	$(error VERSION is required. Usage: make release VERSION=1.4.0)
endif
	@sed -i 's/^version = ".*"/version = "$(VERSION)"/' Cargo.toml
	@cargo check --quiet 2>/dev/null
	@git add Cargo.toml
	@git commit -m "release v$(VERSION)"
	@git tag -a "v$(VERSION)" -m "v$(VERSION)"
	@echo "Tagged v$(VERSION). Push with: git push origin main v$(VERSION)"
