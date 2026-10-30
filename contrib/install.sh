#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# kiiaud installation helper script

set -euo pipefail

MODE="user"

for arg in "$@"; do
    case "$arg" in
        --system)
            MODE="system"
            ;;
        --user)
            MODE="user"
            ;;
        -h|--help)
            echo "Usage: $0 [--user | --system]"
            echo "  --user   Install for current user (~/.local/bin) [default]"
            echo "  --system Install system-wide (/usr/local/bin, requires root/sudo)"
            exit 0
            ;;
        *)
            echo "Unknown option: $arg"
            exit 1
            ;;
    esac
done

echo "==> Building kiiaud in release mode..."
cargo build --release

if [ "$MODE" = "system" ]; then
    if [ "$(id -u)" -ne 0 ]; then
        echo "Error: System-wide installation requires root privileges. Please run with sudo."
        exit 1
    fi
    make install-system
else
    make install-user
fi

