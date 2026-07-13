#include <stdio.h>
#include "hello.h"

#define EXAMPLE_LCD_WIDTH (412)
#define EXAMPLE_LCD_HEIGHT (412)
#define EXAMPLE_LCD_COLOR_BITS (16)

#define ESP_PANEL_HOST_SPI_ID_DEFAULT (SPI2_HOST)
#define ESP_PANEL_LCD_SPI_MODE (0)                  // 0/1/2/3, typically set to 0
#define ESP_PANEL_LCD_SPI_CLK_HZ (80 * 1000 * 1000) // Should be an integer divisor of 80M, typically set to 40M
#define ESP_PANEL_LCD_SPI_TRANS_QUEUE_SZ (10)       // Typically set to 10
#define ESP_PANEL_LCD_SPI_CMD_BITS (32)             // Typically set to 32
#define ESP_PANEL_LCD_SPI_PARAM_BITS (8)            // Typically set to 8

#define ESP_PANEL_LCD_SPI_IO_SCK (40)
#define ESP_PANEL_LCD_SPI_IO_DATA0 (46)
#define ESP_PANEL_LCD_SPI_IO_DATA1 (45)
#define ESP_PANEL_LCD_SPI_IO_DATA2 (42)
#define ESP_PANEL_LCD_SPI_IO_DATA3 (41)
#define ESP_PANEL_LCD_SPI_IO_CS (21)
#define LCD_PIN_NUM_RST (-1) // EXIO2

#define ESP_PANEL_HOST_SPI_MAX_TRANSFER_SIZE (2048)

static const char *TAG = "LCD";

esp_lcd_panel_handle_t panel_handle = NULL;

esp_lcd_panel_handle_t get_panel_handle()
{
    return panel_handle;
}

int QSPI_Init()
{
    static const spi_bus_config_t host_config = {
        .data0_io_num = ESP_PANEL_LCD_SPI_IO_DATA0,
        .data1_io_num = ESP_PANEL_LCD_SPI_IO_DATA1,
        .sclk_io_num = ESP_PANEL_LCD_SPI_IO_SCK,
        .data2_io_num = ESP_PANEL_LCD_SPI_IO_DATA2,
        .data3_io_num = ESP_PANEL_LCD_SPI_IO_DATA3,
        .data4_io_num = -1,
        .data5_io_num = -1,
        .data6_io_num = -1,
        .data7_io_num = -1,
        .max_transfer_sz = ESP_PANEL_HOST_SPI_MAX_TRANSFER_SIZE,
        .flags = SPICOMMON_BUSFLAG_MASTER,
        .intr_flags = 0,
    };
    if (spi_bus_initialize(ESP_PANEL_HOST_SPI_ID_DEFAULT, &host_config, SPI_DMA_CH_AUTO) != ESP_OK)
    {
        ESP_LOGE(TAG, "The SPI initialization failed.");
        return 0;
    }
    ESP_LOGI(TAG, "The SPI initialization succeeded.");

    const esp_lcd_panel_io_spi_config_t io_config = {
        .cs_gpio_num = ESP_PANEL_LCD_SPI_IO_CS,
        .dc_gpio_num = -1,
        .spi_mode = ESP_PANEL_LCD_SPI_MODE,
        .pclk_hz = ESP_PANEL_LCD_SPI_CLK_HZ,
        .trans_queue_depth = ESP_PANEL_LCD_SPI_TRANS_QUEUE_SZ,
        .on_color_trans_done = NULL,
        .user_ctx = NULL,
        .lcd_cmd_bits = ESP_PANEL_LCD_SPI_CMD_BITS,
        .lcd_param_bits = ESP_PANEL_LCD_SPI_PARAM_BITS,
        .flags = {
            .dc_low_on_data = 0,
            .octal_mode = 0,
            .quad_mode = 1,
            .sio_mode = 0,
            .lsb_first = 0,
            .cs_high_active = 0,
        },
    };
    esp_lcd_panel_io_handle_t io_handle = NULL;
    if (esp_lcd_new_panel_io_spi((esp_lcd_spi_bus_handle_t)ESP_PANEL_HOST_SPI_ID_DEFAULT, &io_config, &io_handle) != ESP_OK)
    {
        ESP_LOGE(TAG, "Failed to set LCD communication parameters -- SPI");
        return 0;
    }
    ESP_LOGI(TAG, "LCD communication parameters are set successfully -- SPI\r\n");

    ESP_LOGI(TAG, "Install LCD driver of SPD2010\r\n");
    spd2010_vendor_config_t vendor_config = {
        .flags = {
            .use_qspi_interface = 1,
        },
    };
    esp_lcd_panel_dev_config_t panel_config = {
        .reset_gpio_num = LCD_PIN_NUM_RST,
        .rgb_ele_order = LCD_RGB_ELEMENT_ORDER_RGB,
        // .data_endian = LCD_RGB_DATA_ENDIAN_LITTLE,
        .bits_per_pixel = EXAMPLE_LCD_COLOR_BITS,
        .flags = {
            .reset_active_high = 0,
        },
        .vendor_config = (void *)&vendor_config,
    };
    esp_lcd_new_panel_spd2010(io_handle, &panel_config, &panel_handle);
    esp_lcd_panel_reset(panel_handle);
    esp_lcd_panel_init(panel_handle);
    // esp_lcd_panel_invert_color(panel_handle,false);
    esp_lcd_panel_disp_on_off(panel_handle, true);
    return 1;
}