#!/bin/sh
# One-time setup: create a self-signed "PowerMac Local Signing" code-signing
# certificate in your login keychain. bundle-macos.sh signs with it when it
# exists, so PowerMac.app keeps the same identity across rebuilds and macOS
# keeps its Accessibility / Input Monitoring grants.
#
# This changes your keychain (adds a certificate + private key and marks it
# trusted for code signing), and macOS will ask for your password.
set -e
NAME="PowerMac Local Signing"
if security find-certificate -c "$NAME" >/dev/null 2>&1; then
  echo "\"$NAME\" already exists."
  exit 0
fi
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/cert.cfg" <<CFG
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
CFG
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$TMP/cert.cfg" \
  -keyout "$TMP/key.pem" -out "$TMP/cert.pem" 2>/dev/null
openssl pkcs12 -export -legacy -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
  -out "$TMP/cert.p12" -passout pass:powermac 2>/dev/null \
  || openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" -out "$TMP/cert.p12" -passout pass:powermac
security import "$TMP/cert.p12" -k "$HOME/Library/Keychains/login.keychain-db" -P powermac -T /usr/bin/codesign
security add-trusted-cert -r trustRoot -p codeSign -k "$HOME/Library/Keychains/login.keychain-db" "$TMP/cert.pem"
echo "Created \"$NAME\". Run scripts/bundle-macos.sh, then grant PowerMac.app its permissions one last time."
