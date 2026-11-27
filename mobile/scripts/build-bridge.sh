#!/bin/sh
# Build the Rust bridge for iPhone and the simulator, as ChatBridge.xcframework.
# Run before opening the Xcode project, and again whenever mobile/bridge changes.
set -eu
cd "$(dirname "$0")/.."
export IPHONEOS_DEPLOYMENT_TARGET=17.0
rustup target add aarch64-apple-ios aarch64-apple-ios-sim >/dev/null
(cd bridge && cargo build --release --target aarch64-apple-ios && cargo build --release --target aarch64-apple-ios-sim)
rm -rf ChatBridge.xcframework
xcodebuild -create-xcframework \
  -library bridge/target/aarch64-apple-ios/release/libchat_bridge.a -headers bridge/include \
  -library bridge/target/aarch64-apple-ios-sim/release/libchat_bridge.a -headers bridge/include \
  -output ChatBridge.xcframework
echo "Built mobile/ChatBridge.xcframework"
