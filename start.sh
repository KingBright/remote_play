#!/bin/bash
set -e

# 进入脚本所在目录（项目根目录）
cd "$(dirname "$0")"

TARGET=$1

if [ -z "$TARGET" ]; then
    echo "======================================"
    echo "用法: ./start.sh [host|client|all]"
    echo "示例: ./start.sh host    # 仅启动服务端"
    echo "      ./start.sh client  # 仅启动客户端"
    echo "默认: all (双端全部启动)"
    echo "======================================"
    TARGET="all"
fi

echo "获取 cargo target 目录..."
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps | python3 -c "import sys, json; print(json.load(sys.stdin)['target_directory'])")

function package_and_run() {
    local bin_name=$1
    local app_name=$2
    local is_background=$3
    local bundle_id="com.antigravity.remoteplay.${bin_name}"

    local app_dir="$TARGET_DIR/release/$app_name.app"
    local contents_dir="$app_dir/Contents"
    local macos_dir="$contents_dir/MacOS"
    
    echo "======================================"
    echo "正在编译并打包: $app_name ($bin_name)"
    echo "======================================"
    
    cargo build --release --bin "$bin_name"

    mkdir -p "$macos_dir"
    cp "$TARGET_DIR/release/$bin_name" "$macos_dir/"

    local lsui_element=""
    if [ "$is_background" = "true" ]; then
        lsui_element="<key>LSUIElement</key><true/>"
    fi

    cat <<PLIST > "$contents_dir/Info.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>$bin_name</string>
    <key>CFBundleIdentifier</key>
    <string>$bundle_id</string>
    <key>CFBundleName</key>
    <string>$app_name</string>
    <key>CFBundleVersion</key>
    <string>1.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    $lsui_element
</dict>
</plist>
PLIST

    CERT_NAME="RemotePlay Local"
    if security find-identity -v -p codesigning | grep -q "$CERT_NAME"; then
        echo "使用稳定的本地开发者证书签名 ($CERT_NAME)..."
        codesign --force --deep --sign "$CERT_NAME" --identifier "$bundle_id" "$app_dir"
    else
        echo "⚠️ 未找到稳定的本地开发者证书，降级为 Ad-Hoc 签名。"
        echo "运行 ./setup_stable_signing.sh 并信任证书，以解决 TCC 权限每次构建丢失的问题！"
        codesign --force --deep -s - "$app_dir"
    fi
    
    echo "启动 $app_name..."
    # 强制重新启动 (避免已运行的实例没有被刷新)
    pkill -x "$bin_name" || true
    pkill -f "MacOS/$bin_name" || true
    pkill -f "release/$bin_name" || true
    pkill -f "debug/$bin_name" || true
    sleep 0.5
    
    # 启动应用并将日志重定向到文件
    local log_file="/tmp/${bin_name}.log"
    echo "[$app_name] 日志输出至: $log_file"
    
    # 必须使用 open 启动 .app，否则 macOS 权限系统 (TCC) 无法正确识别 Bundle ID。
    open --stdout "$log_file" --stderr "$log_file" "$app_dir"
    
    echo "$app_name 已启动！"
    echo ""
}

if [ "$TARGET" = "host" ] || [ "$TARGET" = "all" ]; then
    # 1. 编译并启动 Host 端（后台无感运行）
    package_and_run "host" "RemotePlayHost" "true"
fi

if [ "$TARGET" = "client" ] || [ "$TARGET" = "all" ]; then
    # 2. 编译并启动 Client 端（前台带窗口运行）
    package_and_run "client" "RemotePlayClient" "false"
fi

echo "🎉 启动流程结束！"
