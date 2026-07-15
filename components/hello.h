#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "esp_lcd_panel_vendor.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_lcd_panel_handle_t get_panel_handle(void);
int board_display_init(void);
int board_display_set_brightness(uint8_t percent);
int board_touch_init(void);
bool board_touch_read(uint16_t *x, uint16_t *y, uint16_t *strength);
int board_pmu_init(void);
bool board_pmu_take_pkey_long_press(void);
int board_pmu_shutdown(void);
int board_audio_init(void);
int board_audio_read_mic(void *data, int len);
int board_audio_sample_rate(void);

#ifdef __cplusplus
}
#endif
