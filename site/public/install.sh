#!/bin/sh
# AgentStateGraph installer — detects platform and downloads the latest release.
#
# Usage:
#   curl -sSL https://agentstategraph.dev/install.sh | sh
#   curl -sSL https://agentstategraph.dev/install.sh | sh -s -- --dir /usr/local/bin
#
# Options:
#   --dir <path>    Install directory (default: ~/.local/bin)
#   --version <tag> Specific version (default: latest)
#   --help          Show this help

set -e

REPO="agentstatelabs/AgentStateGraph"
BINARY="agentstategraph-mcp"
INSTALL_DIR="${HOME}/.local/bin"
VERSION=""

# Parse args
while [ $# -gt 0 ]; do
  case "$1" in
    --dir) INSTALL_DIR="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --help)
      echo "AgentStateGraph Installer"
      echo ""
      echo "Usage: curl -sSL https://agentstategraph.dev/install.sh | sh"
      echo ""
      echo "Options:"
      echo "  --dir <path>    Install directory (default: ~/.local/bin)"
      echo "  --version <tag> Specific version (default: latest)"
      exit 0
      ;;
    *) shift ;;
  esac
done

# Detect platform
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "${OS}" in
  linux)  OS="unknown-linux-gnu" ;;
  darwin) OS="apple-darwin" ;;
  *)
    echo "Error: Unsupported OS: ${OS}"
    echo "AgentStateGraph supports Linux and macOS. On Windows, use WSL."
    exit 1
    ;;
esac

case "${ARCH}" in
  x86_64|amd64)   ARCH="x86_64" ;;
  aarch64|arm64)   ARCH="aarch64" ;;
  *)
    echo "Error: Unsupported architecture: ${ARCH}"
    exit 1
    ;;
esac

TARGET="${ARCH}-${OS}"

# Get version
if [ -z "${VERSION}" ]; then
  echo "Fetching latest version..."
  VERSION=$(curl -sSf "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
  if [ -z "${VERSION}" ]; then
    # No release yet — try tags
    VERSION=$(curl -sSf "https://api.github.com/repos/${REPO}/tags" | grep '"name"' | head -1 | sed 's/.*"name": *"\([^"]*\)".*/\1/')
  fi
  if [ -z "${VERSION}" ]; then
    echo "Error: Could not determine latest version."
    echo "Try: curl -sSL https://agentstategraph.dev/install.sh | sh -s -- --version v0.3.5-beta.2"
    exit 1
  fi
fi

ARCHIVE="${BINARY}-${VERSION}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"

echo ""
echo "  AgentStateGraph Installer"
echo "  ========================="
echo ""
echo "  Version:   ${VERSION}"
echo "  Platform:  ${TARGET}"
echo "  Install:   ${INSTALL_DIR}/${BINARY}"
echo ""

# Download
echo "Downloading ${URL}..."
TMPDIR=$(mktemp -d)
if ! curl -sSfL "${URL}" -o "${TMPDIR}/${ARCHIVE}"; then
  echo ""
  echo "Error: Download failed."
  echo ""
  echo "This could mean:"
  echo "  - No release has been created yet (run the release workflow first)"
  echo "  - The version '${VERSION}' doesn't exist"
  echo "  - No binary for '${TARGET}' in that release"
  echo ""
  echo "To build from source instead:"
  echo "  git clone https://github.com/${REPO}.git"
  echo "  cd AgentStateGraph"
  echo "  cargo build --release -p agentstategraph-mcp"
  echo ""
  rm -rf "${TMPDIR}"
  exit 1
fi

# Extract
echo "Extracting..."
cd "${TMPDIR}"
tar xzf "${ARCHIVE}"

# Install
mkdir -p "${INSTALL_DIR}"
cp "${BINARY}-${VERSION}-${TARGET}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
chmod +x "${INSTALL_DIR}/${BINARY}"

# Cleanup
rm -rf "${TMPDIR}"

echo ""
echo "  Installed: ${INSTALL_DIR}/${BINARY}"
echo ""

# Check if install dir is in PATH
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo "  Note: ${INSTALL_DIR} is not in your PATH."
    echo "  Add it with:"
    echo ""
    echo "    export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo ""
    echo "  Or add that line to your ~/.bashrc / ~/.zshrc"
    echo ""
    ;;
esac

echo "  Quick start:"
echo ""
echo "    # HTTP API mode"
echo "    ${BINARY} --http --port 3001"
echo "    curl http://localhost:3001/api/health"
echo ""
echo "    # MCP server mode (for Claude Code, GPT, etc.)"
echo "    ${BINARY}"
echo ""
echo "    # Explorer"
echo "    open https://agentstategraph.dev/explorer/"
echo ""
echo "  Docs: https://agentstategraph.dev"
echo ""
