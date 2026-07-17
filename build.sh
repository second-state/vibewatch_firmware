#!/bin/bash

# Build script for esp32-s3-hello.
# Usage: ./build.sh {factory|ota|all}
#
# Partition layout in partitions.csv:
#   nvs      @ 0x9000   - 2M
#   phy_init @ 0x209000 - 4K
#   otadata  @ 0x20a000 - 8K
#   ota_0    @ 0x210000 - 4M rescue/main firmware slot
#   ota_1    @ 0x610000 - 4M rescue/main firmware slot
set -euo pipefail

PT=partitions.csv
TARGET=target/xtensa-esp32s3-espidf/release
APP_BIN=esp32-s3-hello
OTA_IMAGE=./esp32-s3-hello_ota.bin
FACTORY_IMAGE=./esp32-s3-hello_factory.bin

build_main() {
  echo "Building main firmware..."
  cargo build --release --bin "$APP_BIN"
}

save_ota() {
  espflash save-image --chip esp32s3 --flash-size 16mb --partition-table "$PT" \
    --target-app-partition ota_1 "$TARGET/$APP_BIN" "$OTA_IMAGE"
}

save_factory() {
  espflash save-image --chip esp32s3 --merge --flash-size 16mb --partition-table "$PT" \
    --target-app-partition ota_0 "$TARGET/$APP_BIN" "$FACTORY_IMAGE"
}

case "${1:-}" in
  ota)
    build_main
    save_ota
    echo "OTA image: $OTA_IMAGE (main firmware for ota_1)"
    ;;
  factory)
    build_main
    save_factory
    echo "factory image: $FACTORY_IMAGE (main firmware in ota_0; ota_1 left empty)"
    ;;
  all)
    build_main
    save_factory
    save_ota
    echo "factory image: $FACTORY_IMAGE (main firmware in ota_0; ota_1 left empty)"
    echo "OTA image: $OTA_IMAGE (main firmware for ota_1)"
    ;;
  *)
    echo "Usage: $0 {factory|ota|all}"
    echo ""
    echo "  factory - Build merged first-flash image with main firmware in ota_0"
    echo "  ota     - Build OTA update image with main firmware for ota_1"
    echo "  all     - Build both factory and OTA images"
    exit 1
    ;;
esac
