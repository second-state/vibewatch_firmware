#!/bin/bash

# Build script for esp32-s3-hello.
# Usage: ./build.sh {main|ota|main_bin|main_ota_bin|factory|all}
#
# Partition layout in partitions.csv:
#   nvs   @ 0x9000   - 2M
#   ota_0 @ 0x210000 - rescue OTA firmware
#   ota_1 @ 0x410000 - main firmware
set -euo pipefail

PT=partitions.csv
TARGET=target/xtensa-esp32s3-espidf/release
APP_BIN=esp32-s3-hello
MAIN_IMAGE=./esp32-s3-hello_ota.bin
MAIN_MERGED=./esp32-s3-hello.bin
OTA_IMAGE=./ota.bin
FACTORY_IMAGE=./esp32-s3-hello_factory.bin
OTA0_OFFSET=$((0x210000))

build_main() {
  echo "Building main firmware..."
  cargo build --release --bin "$APP_BIN"
}

build_ota() {
  echo "Building OTA rescue firmware..."
  cargo build --release --bin ota
}

save_main_ota() {
  espflash save-image --chip esp32s3 --flash-size 16mb --partition-table "$PT" \
    --target-app-partition ota_1 "$TARGET/$APP_BIN" "$MAIN_IMAGE"
}

save_main_merged() {
  espflash save-image --chip esp32s3 --merge --flash-size 16mb --partition-table "$PT" \
    --target-app-partition ota_1 "$TARGET/$APP_BIN" "$MAIN_MERGED"
}

save_ota() {
  espflash save-image --chip esp32s3 --flash-size 16mb --partition-table "$PT" \
    --target-app-partition ota_0 "$TARGET/ota" "$OTA_IMAGE"
}

case "${1:-}" in
  main)
    build_main
    save_main_ota
    echo "main OTA image: $MAIN_IMAGE"
    ;;
  ota)
    build_ota
    save_ota
    echo "OTA rescue image: $OTA_IMAGE"
    ;;
  main_bin)
    build_main
    save_main_merged
    echo "main merged image: $MAIN_MERGED"
    ;;
  main_ota_bin)
    build_main
    build_ota
    save_main_merged
    save_ota
    dd if="$OTA_IMAGE" of="$MAIN_MERGED" bs=1 seek="$OTA0_OFFSET" conv=notrunc
    echo "main merged image with OTA rescue slot: $MAIN_MERGED"
    ;;
  factory)
    build_main
    build_ota
    save_main_merged
    save_ota
    cp "$MAIN_MERGED" "$FACTORY_IMAGE"
    dd if="$OTA_IMAGE" of="$FACTORY_IMAGE" bs=1 seek="$OTA0_OFFSET" conv=notrunc
    echo "factory image: $FACTORY_IMAGE"
    ;;
  all)
    build_main
    build_ota
    save_main_ota
    save_ota
    echo "main: $MAIN_IMAGE ; ota: $OTA_IMAGE"
    ;;
  *)
    echo "Usage: $0 {main|ota|main_bin|main_ota_bin|factory|all}"
    echo ""
    echo "  main         - Build main OTA image for ota_1"
    echo "  ota          - Build rescue OTA image for ota_0"
    echo "  main_bin     - Build merged main flash image"
    echo "  main_ota_bin - Build merged main image and patch ota_0 rescue image into it"
    echo "  factory      - Build separate factory image containing main + rescue slots"
    echo "  all          - Build main OTA image and rescue OTA image"
    exit 1
    ;;
esac
