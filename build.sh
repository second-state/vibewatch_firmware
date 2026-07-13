#!/bin/bash
# 手表固件构建脚本(双槽 A/B OTA)。Usage: ./build.sh {main|ota|factory|all}
#
#   main    - 主固件整片镜像 watch.bin(含 bootloader+分区表,烧/OTA 上传到 ota_1)
#   ota     - OTA 救援固件镜像 ota.bin(烧到 ota_0,仅首次线缆烧)
#   factory - 含主+救援双槽的整片镜像 watch_factory.bin(首次整片线缆烧)
#   all     - main + ota 都出
#
# 注:日常开发用 `cargo run`(runner 自动烧主固件到 ota_1)。救援固件只在改了 ota.rs 时重建。
set -e

PT=partitions.csv
TARGET=target/xtensa-esp32s3-espidf/release
MAIN_BIN=./watch.bin
OTA_BIN=./ota.bin

build_main() { cargo build --release --bin esp32-s3-hello; }
build_ota()  { cargo build --release --bin ota; }

save_main() {
  espflash save-image --chip esp32s3 --merge --flash-size 16mb --partition-table "$PT" \
    --target-app-partition ota_1 "$TARGET/esp32-s3-hello" "$MAIN_BIN"
}
save_ota() {
  espflash save-image --chip esp32s3 --flash-size 16mb --partition-table "$PT" \
    --target-app-partition ota_0 "$TARGET/ota" "$OTA_BIN"
}

case "${1:-}" in
  main)
    build_main
    save_main
    echo "main image: $MAIN_BIN"
    ;;
  ota)
    build_ota
    save_ota
    echo "ota rescue image: $OTA_BIN"
    ;;
  factory)
    build_main
    build_ota
    save_main          # 整片镜像(含 bootloader+分区表,主固件落在 ota_1)
    save_ota           # 救援固件单槽镜像
    # 把救援固件贴进主镜像的 ota_0 槽(offset 0x50000),得到含双槽的整片镜像。
    dd if="$OTA_BIN" of="$MAIN_BIN" bs=1 seek=$((0x50000)) conv=notrunc
    mv "$MAIN_BIN" ./watch_factory.bin
    echo "factory image (main+rescue): ./watch_factory.bin"
    ;;
  all)
    build_main
    build_ota
    save_main
    save_ota
    echo "main: $MAIN_BIN ; ota: $OTA_BIN"
    ;;
  *)
    echo "Usage: $0 {main|ota|factory|all}"
    exit 1
    ;;
esac
