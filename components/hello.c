#include <stdbool.h>
#include <stdint.h>

#include "esp_err.h"
#include "bsp/esp-bsp.h"
#include "bsp/display.h"
#include "bsp/touch.h"
#include "esp_codec_dev.h"
#include "driver/i2c_master.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_touch.h"
#include "esp_log.h"

static const char *TAG = "board_bridge";

static esp_lcd_panel_handle_t panel_handle = NULL;
static esp_lcd_panel_io_handle_t panel_io_handle = NULL;
static esp_lcd_touch_handle_t touch_handle = NULL;
static esp_codec_dev_handle_t speaker_handle = NULL;
static esp_codec_dev_handle_t microphone_handle = NULL;
static i2c_master_dev_handle_t pmu_handle = NULL;
static bool speaker_opened = false;
static bool microphone_opened = false;

#define BOARD_AUDIO_SAMPLE_RATE (16000)
#define BOARD_AUDIO_BITS_PER_SAMPLE (16)
#define BOARD_AUDIO_CHANNELS (1)

#define AXP2101_I2C_ADDR (0x34)
#define AXP2101_STATUS1 (0x00)
#define AXP2101_COMMON_CONFIG (0x10)
#define AXP2101_INTEN2 (0x41)
#define AXP2101_INTSTS2 (0x49)
#define AXP2101_BAT_PERCENT_DATA (0xA4)
#define AXP2101_PKEY_LONG_IRQ_MASK (1 << 2)

esp_lcd_panel_handle_t get_panel_handle(void)
{
    return panel_handle;
}

esp_lcd_panel_io_handle_t get_panel_io_handle(void)
{
    return panel_io_handle;
}

int board_display_init(void)
{
    if (panel_handle != NULL) {
        return ESP_OK;
    }

    const bsp_display_config_t config = {
        .max_transfer_sz = BSP_LCD_H_RES * BSP_LCD_V_RES * BSP_LCD_BITS_PER_PIXEL / 8,
    };

    esp_err_t err = bsp_display_new(&config, &panel_handle, &panel_io_handle);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "bsp_display_new failed: %s", esp_err_to_name(err));
        return err;
    }

    err = bsp_display_brightness_init();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "bsp_display_brightness_init failed: %s", esp_err_to_name(err));
        return err;
    }

    return ESP_OK;
}

int board_display_set_brightness(uint8_t percent)
{
    if (percent > 100) {
        percent = 100;
    }

    return bsp_display_brightness_set(percent);
}

int board_touch_init(esp_lcd_touch_interrupt_callback_t callback)
{
    if (touch_handle != NULL) {
        return ESP_OK;
    }

    esp_err_t err = bsp_touch_new(NULL, &touch_handle);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "bsp_touch_new failed: %s", esp_err_to_name(err));
        return err;
    }

    if (callback != NULL) {
        err = esp_lcd_touch_register_interrupt_callback(touch_handle, callback);
        if (err != ESP_OK) {
            ESP_LOGE(TAG, "register touch interrupt callback failed: %s", esp_err_to_name(err));
            return err;
        }
    }

    return ESP_OK;
}

static esp_err_t pmu_read_reg(uint8_t reg, uint8_t *data)
{
    if (pmu_handle == NULL || data == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    return i2c_master_transmit_receive(pmu_handle, &reg, 1, data, 1, 100);
}

static esp_err_t pmu_write_reg(uint8_t reg, uint8_t data)
{
    if (pmu_handle == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    uint8_t buffer[2] = {reg, data};
    return i2c_master_transmit(pmu_handle, buffer, sizeof(buffer), 100);
}

int board_pmu_init(void)
{
    if (pmu_handle != NULL) {
        return ESP_OK;
    }

    i2c_master_bus_handle_t bus = bsp_i2c_get_handle();
    if (bus == NULL) {
        ESP_LOGE(TAG, "bsp_i2c_get_handle failed");
        return ESP_FAIL;
    }

    i2c_device_config_t dev_cfg = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address = AXP2101_I2C_ADDR,
        .scl_speed_hz = CONFIG_BSP_I2C_CLK_SPEED_HZ,
    };
    esp_err_t err = i2c_master_bus_add_device(bus, &dev_cfg, &pmu_handle);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "add AXP2101 I2C device failed: %s", esp_err_to_name(err));
        pmu_handle = NULL;
        return err;
    }

    uint8_t int_en2 = 0;
    err = pmu_read_reg(AXP2101_INTEN2, &int_en2);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "read AXP2101 INTEN2 failed: %s", esp_err_to_name(err));
        return err;
    }
    err = pmu_write_reg(AXP2101_INTEN2, int_en2 | AXP2101_PKEY_LONG_IRQ_MASK);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "enable AXP2101 PKEY long IRQ failed: %s", esp_err_to_name(err));
        return err;
    }

    // Clear any stale PKEY long flag from boot.
    uint8_t int_sts2 = 0;
    if (pmu_read_reg(AXP2101_INTSTS2, &int_sts2) == ESP_OK && (int_sts2 & AXP2101_PKEY_LONG_IRQ_MASK)) {
        pmu_write_reg(AXP2101_INTSTS2, AXP2101_PKEY_LONG_IRQ_MASK);
    }

    ESP_LOGI(TAG, "AXP2101 PKEY long-press poweroff enabled");
    return ESP_OK;
}

bool board_pmu_take_pkey_long_press(void)
{
    if (pmu_handle == NULL) {
        return false;
    }

    uint8_t int_sts2 = 0;
    esp_err_t err = pmu_read_reg(AXP2101_INTSTS2, &int_sts2);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "read AXP2101 INTSTS2 failed: %s", esp_err_to_name(err));
        return false;
    }

    if ((int_sts2 & AXP2101_PKEY_LONG_IRQ_MASK) == 0) {
        return false;
    }

    pmu_write_reg(AXP2101_INTSTS2, AXP2101_PKEY_LONG_IRQ_MASK);
    return true;
}

int board_pmu_shutdown(void)
{
    uint8_t value = 0;
    esp_err_t err = pmu_read_reg(AXP2101_COMMON_CONFIG, &value);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "read AXP2101 COMMON_CONFIG failed: %s", esp_err_to_name(err));
        return err;
    }

    ESP_LOGW(TAG, "AXP2101 soft poweroff");
    return pmu_write_reg(AXP2101_COMMON_CONFIG, value | 0x01);
}

int board_pmu_battery_percent(void)
{
    uint8_t status1 = 0;
    esp_err_t err = pmu_read_reg(AXP2101_STATUS1, &status1);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "read AXP2101 STATUS1 failed: %s", esp_err_to_name(err));
        return -1;
    }
    if ((status1 & (1 << 3)) == 0) {
        return -1;
    }

    uint8_t percent = 0;
    err = pmu_read_reg(AXP2101_BAT_PERCENT_DATA, &percent);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "read AXP2101 BAT_PERCENT_DATA failed: %s", esp_err_to_name(err));
        return -1;
    }

    if (percent > 100) {
        percent = 100;
    }
    return percent;
}

bool board_touch_read(uint16_t *x, uint16_t *y, uint16_t *strength)
{
    if (touch_handle == NULL) {
        return false;
    }

    esp_err_t err = esp_lcd_touch_read_data(touch_handle);
    if (err != ESP_OK) {
        if (err == ESP_ERR_INVALID_STATE) {
            return false;
        }
        ESP_LOGW(TAG, "esp_lcd_touch_read_data failed: %s", esp_err_to_name(err));
        return false;
    }

    uint8_t point_count = 0;
    uint16_t local_x = 0;
    uint16_t local_y = 0;
    uint16_t local_strength = 0;
    bool touched = esp_lcd_touch_get_coordinates(
        touch_handle,
        &local_x,
        &local_y,
        &local_strength,
        &point_count,
        1
    );

    if (!touched || point_count == 0) {
        return false;
    }

    if (x != NULL) {
        *x = local_x;
    }
    if (y != NULL) {
        *y = local_y;
    }
    if (strength != NULL) {
        *strength = local_strength;
    }

    return true;
}

int board_audio_init(void)
{
    if (speaker_handle != NULL && microphone_handle != NULL && microphone_opened) {
        return ESP_OK;
    }

    ESP_LOGI(
        TAG,
        "Audio pins: I2C SDA=%d SCL=%d, I2S MCLK=%d BCLK=%d WS=%d DOUT=%d DIN=%d PA=%d",
        BSP_I2C_SDA,
        BSP_I2C_SCL,
        BSP_I2S_MCLK,
        BSP_I2S_SCLK,
        BSP_I2S_LCLK,
        BSP_I2S_DOUT,
        BSP_I2S_DSIN,
        BSP_POWER_AMP_IO
    );

    if (speaker_handle == NULL) {
        speaker_handle = bsp_audio_codec_speaker_init();
        if (speaker_handle == NULL) {
            ESP_LOGE(TAG, "bsp_audio_codec_speaker_init failed");
            return ESP_FAIL;
        }

        int err = esp_codec_dev_set_out_vol(speaker_handle, 50);
        if (err != ESP_CODEC_DEV_OK) {
            ESP_LOGE(TAG, "esp_codec_dev_set_out_vol failed: %d", err);
            return err;
        }
    }

    if (microphone_handle == NULL) {
        microphone_handle = bsp_audio_codec_microphone_init();
        if (microphone_handle == NULL) {
            ESP_LOGE(TAG, "bsp_audio_codec_microphone_init failed");
            return ESP_FAIL;
        }

        int err = esp_codec_dev_set_in_gain(microphone_handle, 30.0f);
        if (err != ESP_CODEC_DEV_OK) {
            ESP_LOGE(TAG, "esp_codec_dev_set_in_gain failed: %d", err);
            return err;
        }
    }

    if (!microphone_opened) {
        esp_codec_dev_sample_info_t fs = {
            .bits_per_sample = BOARD_AUDIO_BITS_PER_SAMPLE,
            .channel = BOARD_AUDIO_CHANNELS,
            .channel_mask = 0,
            .sample_rate = BOARD_AUDIO_SAMPLE_RATE,
            .mclk_multiple = 0,
        };
        int err = esp_codec_dev_open(microphone_handle, &fs);
        if (err != ESP_CODEC_DEV_OK) {
            ESP_LOGE(TAG, "esp_codec_dev_open microphone failed: %d", err);
            return err;
        }
        microphone_opened = true;
    }

    return ESP_OK;
}

int board_audio_close(void)
{
    int first_err = ESP_OK;

    if (speaker_handle != NULL && speaker_opened) {
        int err = esp_codec_dev_close(speaker_handle);
        if (err != ESP_CODEC_DEV_OK) {
            ESP_LOGW(TAG, "esp_codec_dev_close speaker failed: %d", err);
            first_err = err;
        } else {
            speaker_opened = false;
        }
    }

    if (microphone_handle != NULL && microphone_opened) {
        int err = esp_codec_dev_close(microphone_handle);
        if (err != ESP_CODEC_DEV_OK) {
            ESP_LOGW(TAG, "esp_codec_dev_close microphone failed: %d", err);
            if (first_err == ESP_OK) {
                first_err = err;
            }
        } else {
            microphone_opened = false;
        }
    }

    if (first_err == ESP_OK) {
        ESP_LOGI(TAG, "Audio codecs closed");
    }
    return first_err;
}

int board_audio_read_mic(void *data, int len)
{
    if (data == NULL || len <= 0) {
        return ESP_ERR_INVALID_ARG;
    }

    int err = board_audio_init();
    if (err != ESP_OK) {
        return err;
    }

    err = esp_codec_dev_read(microphone_handle, data, len);
    if (err != ESP_CODEC_DEV_OK) {
        return err;
    }

    return len;
}

int board_audio_write_speaker(const void *data, int len)
{
    if (data == NULL || len <= 0) {
        return ESP_ERR_INVALID_ARG;
    }

    int err = board_audio_init();
    if (err != ESP_OK) {
        return err;
    }

    if (!speaker_opened) {
        esp_codec_dev_sample_info_t fs = {
            .bits_per_sample = BOARD_AUDIO_BITS_PER_SAMPLE,
            .channel = BOARD_AUDIO_CHANNELS,
            .channel_mask = 0,
            .sample_rate = BOARD_AUDIO_SAMPLE_RATE,
            .mclk_multiple = 0,
        };
        err = esp_codec_dev_open(speaker_handle, &fs);
        if (err != ESP_CODEC_DEV_OK) {
            ESP_LOGE(TAG, "esp_codec_dev_open speaker failed: %d", err);
            return err;
        }
        speaker_opened = true;
    }

    err = esp_codec_dev_write(speaker_handle, (void *)data, len);
    if (err != ESP_CODEC_DEV_OK) {
        return err;
    }

    return len;
}

int board_audio_sample_rate(void)
{
    return BOARD_AUDIO_SAMPLE_RATE;
}
