#!/bin/bash
set -e

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if [[ -z "${GITHUB_TOKEN}" ]]; then
    echo "Error: GITHUB_TOKEN environment variable not set"
    echo "Get a token from: https://github.com/settings/tokens"
    echo "Required scopes: repo"
    exit 1
fi

REPO="${REPO:-zofrasca/lightclaw}"
VERSION="${1:-}"
RELEASE_NAME="v${VERSION}"
IS_PRERELEASE=false
DIST_DIR="${DIST_DIR:-dist}"

if [[ -z "${VERSION}" ]]; then
    echo "Usage: ./scripts/release.sh <version>"
    echo "Example: ./scripts/release.sh 0.1.0"
    exit 1
fi

if [[ "${VERSION}" == *-* ]]; then
    IS_PRERELEASE=true
fi

echo "Creating release v${VERSION} for ${REPO}..."

"${SCRIPT_DIR}/build.sh" "${VERSION}"

echo ""
echo "Generating SHA-256 checksums..."

sha256_file() {
    local input_file="$1"
    local output_file="${input_file}.sha256"

    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "${input_file}" > "${output_file}"
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "${input_file}" > "${output_file}"
    else
        echo "Error: neither sha256sum nor shasum is installed"
        exit 1
    fi
}

echo ""
echo "Uploading binaries to GitHub..."

api_get() {
    local url="$1"
    curl -sf \
        -H "Authorization: token ${GITHUB_TOKEN}" \
        -H "Accept: application/vnd.github.v3+json" \
        "${url}"
}

api_post_json() {
    local url="$1"
    local data="$2"
    curl -sf -X POST \
        -H "Authorization: token ${GITHUB_TOKEN}" \
        -H "Accept: application/vnd.github.v3+json" \
        "${url}" \
        -d "${data}"
}

json_release_upload_url() {
    local json="$1"
    if command -v jq >/dev/null 2>&1; then
        echo "${json}" | jq -r '.upload_url // empty' | sed 's/{?name,label}//'
    else
        echo "${json}" | tr -d '\n' | sed -n 's/.*"upload_url":"\([^"]*\)".*/\1/p' | sed 's/{?name,label}//'
    fi
}

json_release_id() {
    local json="$1"
    if command -v jq >/dev/null 2>&1; then
        echo "${json}" | jq -r '.id // empty'
    else
        echo "${json}" | tr -d '\n' | sed -n 's/.*"id":[[:space:]]*\([0-9][0-9]*\).*/\1/p'
    fi
}

json_asset_id_by_name() {
    local json="$1"
    local name="$2"
    if command -v jq >/dev/null 2>&1; then
        echo "${json}" | jq -r --arg n "${name}" '.[] | select(.name == $n) | .id' | head -n1
    else
        echo "${json}" | tr -d '\n' \
            | sed -n "s/.*{\"url\":\"[^\"]*\",\"id\":\([0-9][0-9]*\),\"node_id\":\"[^\"]*\",\"name\":\"${name}\".*/\1/p" \
            | head -n1
    fi
}

ASSETS=(
    "lightclaw-linux-x86_64"
    "lightclaw-linux-aarch64"
    "lightclaw-linux-armv7"
    "lightclaw-darwin-x86_64"
    "lightclaw-darwin-aarch64"
    "lightclaw-windows-x86_64.exe"
)

for asset in "${ASSETS[@]}"; do
    if [[ -f "${DIST_DIR}/${asset}" ]]; then
        sha256_file "${DIST_DIR}/${asset}"
    fi
done

UPLOAD_ASSETS=()
for asset in "${ASSETS[@]}"; do
    if [[ -f "${DIST_DIR}/${asset}" ]]; then
        UPLOAD_ASSETS+=("${asset}")
    fi
    checksum_asset="${asset}.sha256"
    if [[ -f "${DIST_DIR}/${checksum_asset}" ]]; then
        UPLOAD_ASSETS+=("${checksum_asset}")
    fi
done

RELEASE_DATA=$(cat <<EOF
{
  "tag_name": "v${VERSION}",
  "name": "${RELEASE_NAME}",
  "body": "lightclaw v${VERSION}\n\nBinaries for Linux (x86_64, aarch64, armv7), macOS, and Windows (Intel and Apple Silicon where supported)",
  "draft": false,
  "prerelease": ${IS_PRERELEASE}
}
EOF
)

RELEASE_RESPONSE=$(api_get "https://api.github.com/repos/${REPO}/releases/tags/v${VERSION}" || true)
if [[ -z "${RELEASE_RESPONSE}" ]]; then
    RELEASE_RESPONSE=$(api_post_json "https://api.github.com/repos/${REPO}/releases" "${RELEASE_DATA}")
fi

UPLOAD_URL="$(json_release_upload_url "${RELEASE_RESPONSE}")"
RELEASE_ID="$(json_release_id "${RELEASE_RESPONSE}")"

if [[ -z "${UPLOAD_URL}" ]] || [[ -z "${RELEASE_ID}" ]]; then
    echo "Error: Failed to load or create release"
    echo "${RELEASE_RESPONSE}"
    exit 1
fi

for asset in "${UPLOAD_ASSETS[@]}"; do
    local_asset="${DIST_DIR}/${asset}"
    if [[ -f "${local_asset}" ]]; then
        release_assets_json="$(api_get "https://api.github.com/repos/${REPO}/releases/${RELEASE_ID}/assets")"
        existing_asset_id="$(json_asset_id_by_name "${release_assets_json}" "${asset}")"
        if [[ -n "${existing_asset_id}" ]]; then
            echo "Removing existing ${asset}..."
            curl -sf -X DELETE \
                -H "Authorization: token ${GITHUB_TOKEN}" \
                -H "Accept: application/vnd.github.v3+json" \
                "https://api.github.com/repos/${REPO}/releases/assets/${existing_asset_id}" > /dev/null
        fi

        echo "Uploading ${asset}..."
        asset_size=$(wc -c < "${local_asset}")

        curl -sf -X POST \
            -H "Authorization: token ${GITHUB_TOKEN}" \
            -H "Accept: application/vnd.github.v3+json" \
            -H "Content-Type: application/octet-stream" \
            -H "Content-Length: ${asset_size}" \
            "${UPLOAD_URL}?name=${asset}" \
            --data-binary "@${local_asset}" > /dev/null

        echo "✓ Uploaded ${asset}"
    else
        echo "⚠ Skipping ${asset} (not found at ${local_asset})"
    fi
done

echo ""
echo "Release v${VERSION} created!"
echo "https://github.com/${REPO}/releases/tag/v${VERSION}"
