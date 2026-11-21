#!/usr/bin/env bash
set -euo pipefail

: "${MACOS_CERTIFICATES_P12:?Missing exported Developer ID identities}"
: "${MACOS_CERTIFICATES_PASSWORD:?Missing certificate export password}"
: "${MACOS_APPLICATION_IDENTITY:?Missing Developer ID Application identity}"
: "${MACOS_INSTALLER_IDENTITY:?Missing Developer ID Installer identity}"
: "${MACOS_NOTARY_KEY_P8:?Missing App Store Connect private key}"
: "${MACOS_NOTARY_KEY_ID:?Missing App Store Connect key ID}"
: "${MACOS_NOTARY_ISSUER_ID:?Missing App Store Connect issuer ID}"

work=$(mktemp -d)
umask 077
export MACOS_KEYCHAIN="$work/signing.keychain-db"
cleanup() {
  /usr/bin/security delete-keychain "$MACOS_KEYCHAIN" >/dev/null 2>&1 || true
  rm -rf -- "$work"
}
trap cleanup EXIT
printf '%s' "$MACOS_CERTIFICATES_P12" | /usr/bin/base64 -D > "$work/identities.p12"
printf '%s' "$MACOS_NOTARY_KEY_P8" > "$work/notary.p8"
password=$(/usr/bin/openssl rand -hex 32)
/usr/bin/security create-keychain -p "$password" "$MACOS_KEYCHAIN"
/usr/bin/security set-keychain-settings -lut 3600 "$MACOS_KEYCHAIN"
/usr/bin/security unlock-keychain -p "$password" "$MACOS_KEYCHAIN"
/usr/bin/security import "$work/identities.p12" -k "$MACOS_KEYCHAIN" \
  -P "$MACOS_CERTIFICATES_PASSWORD" -T /usr/bin/codesign -T /usr/bin/productsign
/usr/bin/security set-key-partition-list -S apple-tool:,apple: -s -k "$password" "$MACOS_KEYCHAIN" >/dev/null
rm -- "$work/identities.p12"
unset MACOS_CERTIFICATES_P12 MACOS_CERTIFICATES_PASSWORD MACOS_NOTARY_KEY_P8

bash "$(dirname "$0")/macos-package.sh" "$1" "$2" "$3" "$work/unsigned.pkg"
/usr/bin/productsign --sign "$MACOS_INSTALLER_IDENTITY" --keychain "$MACOS_KEYCHAIN" \
  --timestamp "$work/unsigned.pkg" "$4"

notary=(--key "$work/notary.p8" --key-id "$MACOS_NOTARY_KEY_ID" --issuer "$MACOS_NOTARY_ISSUER_ID")
xcrun notarytool submit "$4" "${notary[@]}" --wait --timeout 30m \
  --output-format json > "$work/submission.json"
submission=$(jq -er .id "$work/submission.json")
xcrun notarytool log "$submission" "${notary[@]}" "$work/notary-log.json"
cat "$work/notary-log.json"
jq -e '.status == "Accepted"' "$work/submission.json" >/dev/null
xcrun stapler staple "$4"
xcrun stapler validate "$4"
/usr/sbin/pkgutil --check-signature "$4"
