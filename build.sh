#!/bin/bash
set -e

VERSION=${1:-"0.1.0"}
PROJECT_NAME="femtobot"
DIST_DIR="${DIST_DIR:-dist}"

echo "Building femtobot v${VERSION} for all platforms..."
mkdir -p "${DIST_DIR}"

TARGETS=(
    "x86_64-unknown-linux-gnu"
    "aarch64-unknown-linux-gnu"
    "x86_64-apple-darwin"
    "aarch64-apple-darwin"
    "x86_64-pc-windows-gnu"
)

for target in "${TARGETS[@]}"; do
    echo "Building for ${target}..."
    
    # Add target if not already installed
    if ! rustup target list --installed | grep -q "^${target}$"; then
        echo "Adding target ${target}..."
        rustup target add "${target}"
    fi
    
    # Build for target with per-target linker configuration.
    if [[ "${target}" == *"unknown-linux-gnu" ]]; then
        linker="${target}-gcc"
        ar_tool="${target}-ar"
        if ! command -v "${linker}" >/dev/null 2>&1; then
            echo "Missing linker for ${target}: ${linker}"
            echo "Install cross toolchains (Homebrew):"
            echo "  brew tap messense/macos-cross-toolchains"
            echo "  brew install x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu"
            exit 1
        fi

        target_env="${target//-/_}"
        target_env_upper="$(printf '%s' "${target_env}" | tr '[:lower:]' '[:upper:]')"
        env \
            "CC_${target_env}=${linker}" \
            "AR_${target_env}=${ar_tool}" \
            "CARGO_TARGET_${target_env_upper}_LINKER=${linker}" \
            cargo build --release --target "${target}"
    elif [[ "${target}" == *"pc-windows-gnu" ]]; then
        linker="x86_64-w64-mingw32-gcc"
        if ! command -v "${linker}" >/dev/null 2>&1; then
            echo "Missing linker for ${target}: ${linker}"
            echo "Install MinGW toolchain (Homebrew):"
            echo "  brew install mingw-w64"
            exit 1
        fi

        target_env="${target//-/_}"
        target_env_upper="$(printf '%s' "${target_env}" | tr '[:lower:]' '[:upper:]')"
        env \
            "CC_${target_env}=${linker}" \
            "CARGO_TARGET_${target_env_upper}_LINKER=${linker}" \
            cargo build --release --target "${target}"
    else
        cargo build --release --target "${target}"
    fi
    
    # Determine output name
    case "${target}" in
        *-unknown-linux-gnu)
            output_name="${PROJECT_NAME}-linux-${target%%-*}"
            ;;
        *-apple-darwin)
            output_name="${PROJECT_NAME}-darwin-${target%%-*}"
            ;;
        *-pc-windows-gnu)
            output_name="${PROJECT_NAME}-windows-${target%%-*}.exe"
            ;;
        *)
            echo "Unsupported target naming: ${target}"
            exit 1
            ;;
    esac
    
    # Strip binary
    if [[ "${target}" == *"linux"* ]]; then
        strip_tool="${target}-strip"
        if command -v "${strip_tool}" >/dev/null 2>&1; then
            "${strip_tool}" "target/${target}/release/${PROJECT_NAME}" || true
        fi
    elif [[ "${target}" == *"pc-windows-gnu" ]]; then
        strip_tool="x86_64-w64-mingw32-strip"
        if command -v "${strip_tool}" >/dev/null 2>&1; then
            "${strip_tool}" "target/${target}/release/${PROJECT_NAME}.exe" || true
        fi
    fi
    
    # Copy to distribution directory with platform name
    if [[ "${target}" == *"pc-windows-gnu" ]]; then
        cp "target/${target}/release/${PROJECT_NAME}.exe" "${DIST_DIR}/${output_name}"
    else
        cp "target/${target}/release/${PROJECT_NAME}" "${DIST_DIR}/${output_name}"
    fi
    echo "✓ Created ${DIST_DIR}/${output_name}"
done

echo ""
echo "All builds complete!"
echo "Binaries:"
ls -lh "${DIST_DIR}/${PROJECT_NAME}"-* 2>/dev/null || echo "No binaries found"
