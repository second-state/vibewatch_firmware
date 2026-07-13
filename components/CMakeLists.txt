idf_component_register(
     SRCS 
          "./hello.c" 
          "./esp_lcd_spd2010/esp_lcd_spd2010.c"

     INCLUDE_DIRS 
          "." 
          "./esp_lcd_spd2010"

     REQUIRES driver esp_lcd log freertos
)
