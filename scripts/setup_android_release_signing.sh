#!/usr/bin/env bash
set -euo pipefail
SIGN_DIR="${REMOTEPLAY_ANDROID_SIGNING_DIR:-$HOME/Library/Application Support/RemotePlay/Signing}"
KEYSTORE="${REMOTEPLAY_ANDROID_KEYSTORE:-$SIGN_DIR/remoteplay-release.p12}"
ALIAS="${REMOTEPLAY_ANDROID_KEY_ALIAS:-remoteplay}"
SERVICE="com.remoteplay.android.release.signing"
ACCOUNT="${USER:-remoteplay}"
command -v keytool >/dev/null 2>&1 || { echo 'keytool is required' >&2; exit 1; }
command -v security >/dev/null 2>&1 || { echo 'macOS security command is required' >&2; exit 1; }
command -v openssl >/dev/null 2>&1 || { echo 'openssl is required' >&2; exit 1; }
mkdir -p "$SIGN_DIR" && chmod 700 "$SIGN_DIR"
if [ -f "$KEYSTORE" ]; then
  security find-generic-password -a "$ACCOUNT" -s "$SERVICE" -w >/dev/null 2>&1 || { echo "Keystore exists but Keychain password is missing: $KEYSTORE" >&2; exit 2; }
  echo "Release signing already configured: $KEYSTORE"
  exit 0
fi
PASSWORD="$(openssl rand -hex 32)"
umask 077
keytool -genkeypair -storetype PKCS12 -keystore "$KEYSTORE" -storepass "$PASSWORD" -keypass "$PASSWORD" -alias "$ALIAS" -keyalg RSA -keysize 4096 -sigalg SHA256withRSA -validity 10000 -dname 'CN=RemotePlay Android Release, O=RemotePlay'
chmod 600 "$KEYSTORE"
security add-generic-password -U -a "$ACCOUNT" -s "$SERVICE" -w "$PASSWORD" >/dev/null
unset PASSWORD
echo "Created RemotePlay Android release signing identity."
echo "Keystore: $KEYSTORE"
echo "Password stored in macOS Keychain service: $SERVICE"
