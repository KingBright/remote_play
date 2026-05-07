#!/bin/bash
set -e

CERT_NAME="RemotePlay Local"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

echo "===================================================="
echo "   RemotePlay Stable Signing Setup (Local Developer)"
echo "===================================================="

# Check if identity already exists
if security find-identity -v -p codesigning | grep -q "$CERT_NAME"; then
    echo "✅ Identity '$CERT_NAME' already exists in Keychain."
else
    echo "Creating a persistent self-signed identity for RemotePlay..."
    
    TMP_DIR=$(mktemp -d)
    pushd "$TMP_DIR" > /dev/null

    # 1. Generate a self-signed cert with Code Signing extensions
    # We use a config file to ensure extendedKeyUsage is set
    cat <<EOF > cert.conf
[req]
distinguished_name = req_distinguished_name
prompt = no
x509_extensions = v3_ca

[req_distinguished_name]
CN = $CERT_NAME

[v3_ca]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = codeSigning
EOF

    openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
        -keyout dev.key -out dev.crt -config cert.conf -extensions v3_ca

    # 2. Export to PKCS#12 (LibreSSL compatibility)
    openssl pkcs12 -export -legacy \
        -in dev.crt -inkey dev.key \
        -out dev.p12 -passout pass:remoteplay -name "$CERT_NAME"

    # 3. Import into login keychain
    echo "Importing identity to login keychain..."
    security import dev.p12 -k "$KEYCHAIN" -P remoteplay -T /usr/bin/codesign

    popd > /dev/null
    rm -rf "$TMP_DIR"
    
    echo "✅ Identity imported successfully."
fi

echo ""
echo "CRITICAL STEP REQUIRED:"
echo "----------------------------------------------------"
echo "1. Open 'Keychain Access' app (钥匙串访问)."
echo "2. Find '$CERT_NAME' under 'login' (登录) -> 'My Certificates' (我的证书)."
echo "3. Double-click it, expand the 'Trust' (信任) section."
echo "4. Change 'When using this certificate' to 'Always Trust' (始终信任)."
echo "5. Close the window and enter your Mac password."
echo "----------------------------------------------------"
echo "Once done, your permissions will persist across builds!"
echo "===================================================="
