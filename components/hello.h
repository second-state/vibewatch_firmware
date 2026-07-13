#include "esp_lcd_panel_vendor.h"
#include "driver/spi_master.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_io_interface.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_panel_vendor.h"
#include "esp_log.h"
#include "esp_lcd_spd2010.h"

esp_lcd_panel_handle_t get_panel_handle();
int QSPI_Init();