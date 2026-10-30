# ==============================================================================
# kiiaud — Build & Installation Makefile
# ==============================================================================

CARGO ?= cargo
CARGO_FLAGS ?= --release

PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
SYSCONFDIR ?= /etc/kiiaud
SYSTEMD_SYSTEM_DIR ?= /etc/systemd/system

USER_HOME ?= $(HOME)
USER_BINDIR ?= $(USER_HOME)/.local/bin
USER_CONFIGDIR ?= $(USER_HOME)/.config/kiiaud
USER_SYSTEMD_DIR ?= $(USER_HOME)/.config/systemd/user

TARGET_DIR ?= target/release
BINARIES = $(TARGET_DIR)/kiiaud $(TARGET_DIR)/kiiaudctl

.PHONY: all build check test clean install install-user install-system uninstall-user uninstall-system help

all: build

help:
	@echo "kiiaud build and installation helper"
	@echo ""
	@echo "Targets:"
	@echo "  build             Build optimized release binaries (kiiaud, kiiaudctl)"
	@echo "  check             Run compiler lint checks"
	@echo "  test              Execute all automated unit and integration tests"
	@echo "  install-user      Install binaries to ~/.local/bin and init ~/.config/kiiaud"
	@echo "  install-system    Install binaries to /usr/local/bin and /etc/kiiaud (requires sudo)"
	@echo "  uninstall-user    Remove installed binaries from ~/.local/bin"
	@echo "  uninstall-system  Remove installed binaries from /usr/local/bin"
	@echo "  clean             Remove target/ compilation artifacts"

build:
	@echo "==> Building kiiaud (release mode)..."
	$(CARGO) build $(CARGO_FLAGS)

check:
	$(CARGO) check --all-targets

test:
	$(CARGO) test --workspace

clean:
	$(CARGO) clean

# ------------------------------------------------------------------------------
# Per-User Local Installation (~/.local/bin)
# ------------------------------------------------------------------------------
install-user: build
	@echo "==> Installing kiiaud to user directory: $(USER_BINDIR)..."
	@mkdir -p $(USER_BINDIR)
	@install -m 0755 $(TARGET_DIR)/kiiaud $(USER_BINDIR)/kiiaud
	@install -m 0755 $(TARGET_DIR)/kiiaudctl $(USER_BINDIR)/kiiaudctl
	@echo "==> Initializing user config directory: $(USER_CONFIGDIR)..."
	@mkdir -p $(USER_CONFIGDIR)
	@if [ ! -f $(USER_CONFIGDIR)/config.toml ]; then \
		echo "==> Generating initial default configuration at $(USER_CONFIGDIR)/config.toml..."; \
		$(USER_BINDIR)/kiiaud --generate-config $(USER_CONFIGDIR)/config.toml; \
	else \
		echo "==> Existing config found at $(USER_CONFIGDIR)/config.toml (keeping untouched)"; \
	fi
	@echo "==> Installing systemd user service..."
	@mkdir -p $(USER_SYSTEMD_DIR)
	@install -m 0644 contrib/systemd/kiiaud-user.service $(USER_SYSTEMD_DIR)/kiiaud.service
	@echo ""
	@echo "Installation successful!"
	@echo "Make sure '$(USER_BINDIR)' is in your PATH. You can run:"
	@echo "  kiiaud --check-config"
	@echo "  systemctl --user daemon-reload"
	@echo "  systemctl --user enable --now kiiaud"

uninstall-user:
	@echo "==> Removing user binaries..."
	@rm -f $(USER_BINDIR)/kiiaud $(USER_BINDIR)/kiiaudctl
	@rm -f $(USER_SYSTEMD_DIR)/kiiaud.service
	@echo "Uninstallation complete. (Config directory $(USER_CONFIGDIR) preserved)."

# ------------------------------------------------------------------------------
# System-wide Installation (/usr/local/bin)
# ------------------------------------------------------------------------------
install-system: build
	@echo "==> Installing kiiaud system-wide to $(BINDIR)..."
	@mkdir -p $(DESTDIR)$(BINDIR)
	@install -m 0755 $(TARGET_DIR)/kiiaud $(DESTDIR)$(BINDIR)/kiiaud
	@install -m 0755 $(TARGET_DIR)/kiiaudctl $(DESTDIR)$(BINDIR)/kiiaudctl
	@echo "==> Setting up system configuration directory: $(SYSCONFDIR)..."
	@mkdir -p $(DESTDIR)$(SYSCONFDIR)
	@if [ ! -f $(DESTDIR)$(SYSCONFDIR)/config.toml ]; then \
		echo "==> Installing example system config to $(SYSCONFDIR)/config.toml..."; \
		install -m 0640 contrib/kiiaud.toml.example $(DESTDIR)$(SYSCONFDIR)/config.toml; \
	else \
		echo "==> Existing config found at $(SYSCONFDIR)/config.toml (keeping untouched)"; \
	fi
	@echo "==> Installing systemd service unit to $(SYSTEMD_SYSTEM_DIR)..."
	@mkdir -p $(DESTDIR)$(SYSTEMD_SYSTEM_DIR)
	@install -m 0644 contrib/systemd/kiiaud.service $(DESTDIR)$(SYSTEMD_SYSTEM_DIR)/kiiaud.service
	@echo ""
	@echo "System installation successful!"
	@echo "You can manage the service via:"
	@echo "  systemctl daemon-reload"
	@echo "  systemctl enable --now kiiaud"

# Alias for standard make install
install: install-system

uninstall-system:
	@echo "==> Removing system binaries..."
	@rm -f $(DESTDIR)$(BINDIR)/kiiaud $(DESTDIR)$(BINDIR)/kiiaudctl
	@rm -f $(DESTDIR)$(SYSTEMD_SYSTEM_DIR)/kiiaud.service
	@echo "System uninstallation complete."

