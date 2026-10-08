#!/usr/bin/env bash
# Install the EchoAgent desktop updater static server on 123.56.188.16.
set -euo pipefail

NGINX_SITE="/etc/nginx/sites-enabled/echo-agent-server-https"
NGINX_SNIPPET="/etc/nginx/snippets/echoagent-desktop-updates.conf"
PUBLISHER="/usr/local/sbin/echoagent-publish-update"
UPDATE_ROOT="/opt/echo-agent-desktop-updates"
BACKUP_DIR="/etc/nginx/echoagent-backups"
INCLUDE_LINE="    include /etc/nginx/snippets/echoagent-desktop-updates.conf;"

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  echo "This installer must run as root." >&2
  exit 1
fi
if [[ ! -f /tmp/echoagent-publish-update || ! -f /tmp/echoagent-desktop-updates-nginx.conf ]]; then
  echo "Upload the publisher and Nginx snippet to /tmp before running this installer." >&2
  exit 1
fi

install -d -o root -g root -m 0755 "$UPDATE_ROOT/stable/generations" "$UPDATE_ROOT/releases"
CREATED_CURRENT=0
if [[ ! -L "$UPDATE_ROOT/stable/current" ]]; then
  if [[ -e "$UPDATE_ROOT/stable/current" ]]; then
    echo "stable/current exists but is not a symlink; refusing to overwrite it." >&2
    exit 1
  fi
  # Preserve manifests published by the former single-target publisher.
  GENERATION="$(mktemp -d "$UPDATE_ROOT/stable/generations/initial.XXXXXX")"
  shopt -s nullglob
  for manifest in "$UPDATE_ROOT/stable/"*.json; do
    install -o root -g root -m 0644 "$manifest" "$GENERATION/$(basename "$manifest")"
  done
  shopt -u nullglob
  chmod 0755 "$GENERATION"
  ln -s "generations/$(basename "$GENERATION")" "$UPDATE_ROOT/stable/.current-install"
  mv -Tf "$UPDATE_ROOT/stable/.current-install" "$UPDATE_ROOT/stable/current"
  CREATED_CURRENT=1
fi
install -d -o root -g root -m 0700 "$BACKUP_DIR"
BACKUP="$(mktemp -d "$BACKUP_DIR/install.XXXXXX")"
[[ ! -f "$PUBLISHER" ]] || cp -p "$PUBLISHER" "$BACKUP/publisher"
[[ ! -f "$NGINX_SNIPPET" ]] || cp -p "$NGINX_SNIPPET" "$BACKUP/snippet"
cp -p "$NGINX_SITE" "$BACKUP/site"

restore_file() {
  local backup="$1" destination="$2"
  if [[ -f "$backup" ]]; then cp -p "$backup" "$destination"
  else rm -f "$destination"
  fi
}

install -o root -g root -m 0755 /tmp/echoagent-publish-update "$PUBLISHER"
install -o root -g root -m 0644 /tmp/echoagent-desktop-updates-nginx.conf "$NGINX_SNIPPET"

if ! grep -Fq "$INCLUDE_LINE" "$NGINX_SITE"; then
  sed -i "\|^[[:space:]]*location / {|i\\$INCLUDE_LINE" "$NGINX_SITE"
fi

if ! nginx -t || ! systemctl reload nginx; then
  restore_file "$BACKUP/site" "$NGINX_SITE"
  restore_file "$BACKUP/snippet" "$NGINX_SNIPPET"
  restore_file "$BACKUP/publisher" "$PUBLISHER"
  if [[ $CREATED_CURRENT -eq 1 ]]; then rm -f "$UPDATE_ROOT/stable/current"; fi
  nginx -t || true
  systemctl reload nginx || true
  echo "Nginx install failed; restored the previous server files." >&2
  exit 1
fi

echo "EchoAgent desktop update server installed."
echo "Root: $UPDATE_ROOT"
