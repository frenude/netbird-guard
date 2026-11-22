#!/usr/bin/env sh
set -eu

# nbg (NetBird Guard) one-line installer
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/frenude/netbird-guard/main/install.sh | sh
#
# Optional environment variables to pre-fill ~/.config/nbg/config.toml:
#   FEISHU_WEBHOOK_URL   Custom Feishu group bot webhook URL
#   FEISHU_SECRET        Optional signing secret for webhook
#   FEISHU_APP_ID        Feishu Enterprise App ID (direct DM mode)
#   FEISHU_APP_SECRET    Feishu Enterprise App Secret
#   FEISHU_OPEN_ID       Target Feishu user open_id
#   DEVICE_NAME          Custom device label (defaults to hostname)

REPO="frenude/netbird-guard"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
  x86_64|amd64) ARCH="x86_64" ;;
  arm64|aarch64) ARCH="aarch64" ;;
  *) echo "❌ Unsupported architecture: $ARCH"; exit 1 ;;
esac

case "$OS" in
  darwin) ASSET="nbg-darwin-${ARCH}" ;;
  linux)  ASSET="nbg-linux-${ARCH}" ;;
  *) echo "❌ Unsupported OS: $OS"; exit 1 ;;
esac

INSTALL_DIR="/usr/local/bin"
[ "$OS" = "darwin" ] && [ -w "/opt/homebrew/bin" ] && INSTALL_DIR="/opt/homebrew/bin"

DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
TMP_FILE="$(mktemp)"
trap 'rm -f "$TMP_FILE"' EXIT

echo "⬇️  Downloading ${ASSET} from GitHub Releases..."
curl -fsSL "$DOWNLOAD_URL" -o "$TMP_FILE"
chmod +x "$TMP_FILE"

if [ -w "$INSTALL_DIR" ]; then
  rm -f "${INSTALL_DIR}/nbg" && mv "$TMP_FILE" "${INSTALL_DIR}/nbg"
else
  echo "🔐 Installing to ${INSTALL_DIR}/nbg (sudo required)..."
  sudo mkdir -p "$INSTALL_DIR"
  sudo rm -f "${INSTALL_DIR}/nbg" && sudo mv "$TMP_FILE" "${INSTALL_DIR}/nbg"
fi

CFG_DIR="${HOME}/.config/nbg"
CFG_FILE="${CFG_DIR}/config.toml"
mkdir -p "$CFG_DIR"

if [ ! -f "$CFG_FILE" ] || [ -n "${FEISHU_WEBHOOK_URL:-}${FEISHU_APP_ID:-}" ]; then
  AUTO_OPEN="false"
  [ "$OS" = "darwin" ] && AUTO_OPEN="true"
  cat > "$CFG_FILE" <<EOF
# nbg (NetBird Guard) config
device_name = "${DEVICE_NAME:-}"
netbird_bin = ""
management_url = ""
check_interval_secs = 60
renew_before_secs = 0
notify_cooldown_secs = 1800
login_wait_timeout_secs = 300
auto_open_browser = ${AUTO_OPEN}
macos_notification = ${AUTO_OPEN}
notify_on_recovery = false

[feishu]
# Option 1: Feishu custom group bot webhook
webhook_url = "${FEISHU_WEBHOOK_URL:-}"
secret = "${FEISHU_SECRET:-}"

# Option 2: Feishu Enterprise App direct DM (no relay server required)
app_id = "${FEISHU_APP_ID:-}"
app_secret = "${FEISHU_APP_SECRET:-}"
receive_open_id = "${FEISHU_OPEN_ID:-}"
EOF
  chmod 600 "$CFG_FILE"
  echo "⚙️  Wrote config: ${CFG_FILE}"
else
  echo "ℹ️  Keeping existing config: ${CFG_FILE}"
fi

echo "🚀 Installing & starting background daemon..."
if [ "$OS" = "linux" ] && [ "$(id -u)" -ne 0 ]; then
  sudo "${INSTALL_DIR}/nbg" install --config "$CFG_FILE"
else
  "${INSTALL_DIR}/nbg" install
fi

echo ""
echo "✅ nbg installed!"
echo "   Config : ${CFG_FILE}"
echo "   Status : nbg status"
echo "   Test   : nbg test"
echo "   Remove : nbg uninstall"
