.PHONY: all cli host install clean

all: cli host

cli:
	cargo build --release --manifest-path cli/Cargo.toml

host:
	./scripts/bundle-host.sh

install: all
	-./cli/target/release/shovel host stop 2>/dev/null
	mkdir -p $(HOME)/Applications
	rm -rf $(HOME)/Applications/Shovel.app
	cp -R build/Shovel.app $(HOME)/Applications/Shovel.app
	cargo install --path cli --root $(HOME)/.cargo --force

clean:
	rm -rf build cli/target host/.build
