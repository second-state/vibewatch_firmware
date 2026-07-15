#include <stdbool.h>
#include <stdint.h>

#include "esp_err.h"
#include "bsp/esp-bsp.h"
#include "bsp/display.h"
#include "bsp/touch.h"
#include "esp_codec_dev.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_touch.h"
#include "esp_log.h"

static const char *TAG = "board_bridge";

static esp_lcd_panel_handle_t panel_handle = NULL;
static esp_lcd_panel_io_handle_t panel_io_handle = NULL;
static esp_lcd_touch_handle_t touch_handle = NULL;
static esp_codec_dev_handle_t speaker_handle = NULL;
static esp_codec_dev_handle_t microphone_handle = NULL;
static bool microphone_opened = false;

#define BOARD_AUDIO_SAMPLE_RATE (16000)
#define BOARD_AUDIO_BITS_PER_SAMPLE (16)
#define BOARD_AUDIO_CHANNELS (1)

esp_lcd_panel_handle_t get_panel_handle(void)
{
    return panel_handle;
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

int board_touch_init(void)
{
    if (touch_handle != NULL) {
        return ESP_OK;
    }

    esp_err_t err = bsp_touch_new(NULL, &touch_handle);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "bsp_touch_new failed: %s", esp_err_to_name(err));
    }
    return err;
}

bool board_touch_read(uint16_t *x, uint16_t *y, uint16_t *strength)
{
    if (touch_handle == NULL) {
        return false;
    }

    esp_err_t err = esp_lcd_touch_read_data(touch_handle);
    if (err != ESP_OK) {
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

int board_audio_sample_rate(void)
{
    return BOARD_AUDIO_SAMPLE_RATE;
}
