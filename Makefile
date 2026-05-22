BINARY := target/release/laramux
LOCAL_BIN ?= $(HOME)/.local/bin

.PHONY: build install clean

build:
	@cargo build --release
	@echo "Binary at $(BINARY)"

install: build
	@install -Dm755 $(BINARY) $(LOCAL_BIN)/laramux
	@echo "Installed to $(LOCAL_BIN)/laramux"

clean:
	@cargo clean
