idf_component_register(
     SRCS
          "./board.c"

     INCLUDE_DIRS
          "."

     REQUIRES driver esp_lcd log freertos waveshare__esp32_s3_touch_amoled_2_06 espressif__esp_lcd_touch espressif__esp_codec_dev
)
