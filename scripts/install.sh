#!/bin/bash
set -e

VERSION="${VERSION:-latest}"
REPO="${REPO:-enzofrasca/femtobot}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
BINARY_NAME="femtobot"
TEMP_DIR=$(mktemp -d)
cleanup() { rm -rf "${TEMP_DIR}"; }
trap cleanup EXIT

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; }

detect_platform() {
    OS=$(uname -s 2>/dev/null || echo "unknown")
    ARCH=$(uname -m 2>/dev/null || echo "unknown")

    case "${OS}" in
        Linux*)  OS_TYPE="linux" ;;
        Darwin*) OS_TYPE="darwin" ;;
        FreeBSD*) OS_TYPE="freebsd" ;;
        *)       OS_TYPE="unknown" ;;
    esac

    case "${ARCH}" in
        x86_64|amd64)    ARCH_TYPE="x86_64" ;;
        aarch64|arm64)   ARCH_TYPE="aarch64" ;;
        armv7l|armv7)    ARCH_TYPE="armv7" ;;
        i386|i686)       ARCH_TYPE="i686" ;;
        *)               ARCH_TYPE="unknown" ;;
    esac

    echo "${OS_TYPE}-${ARCH_TYPE}"
}

check_dependencies() {
    if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
        error "Neither curl nor wget found. Please install one of them."
        exit 1
    fi
}

download() {
    local url="$1"
    local output="$2"

    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "${url}" -o "${output}" 2>/dev/null || curl -fL "${url}" -o "${output}"
    else
        wget -qO "${output}" "${url}" || wget -O "${output}" "${url}"
    fi
}

get_binary_name() {
    local platform="$1"
    local os_type="${platform%%-*}"
    local arch_type="${platform##*-}"
    echo "femtobot-${os_type}-${arch_type}"
}

resolve_download_url() {
    local version="$1"
    local release_tag="$version"

    if [[ "${version}" == "latest" ]]; then
        echo "https://github.com/${REPO}/releases/latest/download"
        return
    fi

    if [[ "${release_tag}" != v* ]]; then
        release_tag="v${release_tag}"
    fi
    echo "https://github.com/${REPO}/releases/download/${release_tag}"
}

is_supported_platform() {
    local platform="$1"
    case "${platform}" in
        linux-x86_64|linux-aarch64|darwin-x86_64|darwin-aarch64)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

verify_checksum() {
    local asset_name="$1"
    local downloaded_file="$2"
    local checksum_file="${TEMP_DIR}/${asset_name}.sha256"
    local checksum_url="${DOWNLOAD_URL}/${asset_name}.sha256"

    if ! download "${checksum_url}" "${checksum_file}" >/dev/null 2>&1; then
        warn "Checksum file not found (${asset_name}.sha256). Skipping checksum verification."
        return
    fi

    local expected actual
    expected="$(awk '{print $1}' "${checksum_file}")"
    if [[ -z "${expected}" ]]; then
        error "Checksum file is empty or malformed: ${checksum_file}"
        exit 1
    fi

    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "${downloaded_file}" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "${downloaded_file}" | awk '{print $1}')"
    else
        warn "No SHA-256 tool found (sha256sum/shasum). Skipping checksum verification."
        return
    fi

    if [[ "${expected}" != "${actual}" ]]; then
        error "Checksum mismatch for ${asset_name}"
        error "Expected: ${expected}"
        error "Actual:   ${actual}"
        exit 1
    fi

    info "Checksum verified for ${asset_name}"
}

create_config() {
    local config_dir="$HOME/.femtobot"
    local config_file="${config_dir}/config.json"
    local data_dir="${config_dir}/data"
    local workspace_dir="${config_dir}/workspace"

    mkdir -p "${config_dir}" "${data_dir}" "${workspace_dir}"

    if [[ -f "${config_file}" ]]; then
        info "Found existing config at ${config_file}"
        return
    fi

    info "Creating empty configuration..."

    cat > "${config_file}" <<EOF
{
  "providers": {
    "openrouter": {
      "apiKey": "",
      "apiBase": "https://openrouter.ai/api/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "anthropic/claude-opus-4-5"
    }
  },
  "channels": {
    "telegram": {
      "token": "",
      "allow_from": []
    }
  },
  "tools": {
    "web": {
      "search": {
        "apiKey": ""
      }
    },
    "exec": {
      "timeout": 60
    },
    "restrict_to_workspace": false
  }
}
EOF

    info "Config created at ${config_file}"
    info "Run: femtobot configure"
}

setup_service() {
    if [[ "${OS_TYPE}" == "linux" ]]; then
        if ! command -v systemctl >/dev/null 2>&1; then
            error "systemctl is required on Linux for background daemon mode"
            exit 1
        fi

        local service_dir="$HOME/.config/systemd/user"
        mkdir -p "${service_dir}"

        cat > "${service_dir}/femtobot.service" <<EOF
[Unit]
Description=femtobot Telegram Bot
After=network.target

[Service]
Type=simple
ExecStart=${INSTALL_DIR}/${BINARY_NAME}
WorkingDirectory=${HOME}
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
EOF

        if ! systemctl --user daemon-reload; then
            error "Failed to reload systemd user daemon"
            exit 1
        fi
        if ! systemctl --user enable --now femtobot; then
            error "Failed to enable/start femtobot service"
            error "If running over SSH, user services may need login session support."
            exit 1
        fi

        info "Daemon enabled and started with systemd user service"
        return
    fi

    if [[ "${OS_TYPE}" == "darwin" ]]; then
        local launch_dir="$HOME/Library/LaunchAgents"
        local logs_dir="$HOME/.femtobot/logs"
        local plist="${launch_dir}/io.femtobot.agent.plist"
        local label="io.femtobot.agent"
        local uid
        uid="$(id -u)"

        mkdir -p "${launch_dir}" "${logs_dir}"

        cat > "${plist}" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>${INSTALL_DIR}/${BINARY_NAME}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>WorkingDirectory</key>
  <string>${HOME}</string>
  <key>StandardOutPath</key>
  <string>${logs_dir}/femtobot.log</string>
  <key>StandardErrorPath</key>
  <string>${logs_dir}/femtobot.error.log</string>
</dict>
</plist>
EOF

        launchctl bootout "gui/${uid}" "${plist}" >/dev/null 2>&1 || true
        if ! launchctl bootstrap "gui/${uid}" "${plist}"; then
            error "Failed to bootstrap launchd agent"
            exit 1
        fi
        if ! launchctl kickstart -k "gui/${uid}/${label}"; then
            error "Failed to start launchd agent"
            exit 1
        fi

        info "Daemon enabled and started with launchd agent"
        return
    fi
}

main() {
    echo ""
    echo " femtobot Installer"
    echo "===================="
    echo ""

    check_dependencies

    PLATFORM=$(detect_platform)
    OS_TYPE="${PLATFORM%%-*}"
    ARCH_TYPE="${PLATFORM##*-}"
    DOWNLOAD_URL="$(resolve_download_url "${VERSION}")"

    if [[ "${OS_TYPE}" == "unknown" ]] || [[ "${ARCH_TYPE}" == "unknown" ]]; then
        error "Unable to detect your platform: $(uname -s) $(uname -m)"
        error "Supported platforms: linux-x86_64, linux-aarch64, darwin-x86_64, darwin-aarch64"
        exit 1
    fi

    if ! is_supported_platform "${PLATFORM}"; then
        error "Unsupported platform: ${PLATFORM}"
        error "Supported platforms: linux-x86_64, linux-aarch64, darwin-x86_64, darwin-aarch64"
        exit 1
    fi

    info "Detected platform: ${PLATFORM}"
    info "Binary name: $(get_binary_name "${PLATFORM}")"

    BINARY_FILE=$(get_binary_name "${PLATFORM}")
    DOWNLOAD_FILE="${TEMP_DIR}/${BINARY_NAME}"

    info "Downloading from ${DOWNLOAD_URL}/${BINARY_FILE}..."

    if ! download "${DOWNLOAD_URL}/${BINARY_FILE}" "${DOWNLOAD_FILE}"; then
        error "Failed to download binary"
        error "Please check your internet connection or visit: ${DOWNLOAD_URL}"
        exit 1
    fi

    if [[ ! -f "${DOWNLOAD_FILE}" ]]; then
        error "Downloaded file not found"
        exit 1
    fi

    verify_checksum "${BINARY_FILE}" "${DOWNLOAD_FILE}"

    chmod +x "${DOWNLOAD_FILE}"

    mkdir -p "${INSTALL_DIR}"
    mv "${DOWNLOAD_FILE}" "${INSTALL_DIR}/${BINARY_NAME}"

    if [[ ! -f "${INSTALL_DIR}/${BINARY_NAME}" ]]; then
        error "Failed to install binary to ${INSTALL_DIR}"
        exit 1
    fi

    info "Binary installed to ${INSTALL_DIR}/${BINARY_NAME}"

    if [[ ":$PATH:" != *":${INSTALL_DIR}:"* ]]; then
        warn "${INSTALL_DIR} is not in your PATH"
        warn "Add the following to your shell profile:"
        warn "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    fi

    create_config
    setup_service

    echo ""
    info "Installation complete!"
    echo ""
    echo "Binary: ${INSTALL_DIR}/${BINARY_NAME}"
    echo "Config: $HOME/.femtobot/config.json"
    echo ""
    if [[ "${OS_TYPE}" == "linux" ]] && [[ -f "$HOME/.config/systemd/user/femtobot.service" ]]; then
        echo "Service status: systemctl --user status femtobot"
        echo "Service logs:  journalctl --user -u femtobot -f"
    elif [[ "${OS_TYPE}" == "darwin" ]] && [[ -f "$HOME/Library/LaunchAgents/io.femtobot.agent.plist" ]]; then
        echo "Service status: launchctl print gui/$(id -u)/io.femtobot.agent"
        echo "Service logs:  tail -f $HOME/.femtobot/logs/femtobot.log"
    else
        echo "Run: ${BINARY_NAME}"
    fi
    echo ""
}

main "$@"
