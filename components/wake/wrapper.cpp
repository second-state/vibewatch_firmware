/* Edge Impulse ingestion SDK
 * Copyright (c) 2022 EdgeImpulse Inc.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 */

#ifndef _INFERENCE_H
#define _INFERENCE_H

// Undefine min/max macros as these conflict with C++ std min/max functions
// these are often included by Arduino cores
#ifdef min
#undef min
#endif // min
#ifdef max
#undef max
#endif // max
#ifdef round
#undef round
#endif // round
// Similar the ESP32 seems to define this, which is also used as an enum value in TFLite
#ifdef DEFAULT
#undef DEFAULT
#endif // DEFAULT
// Infineon core defines this, conflicts with CMSIS/DSP/Include/dsp/controller_functions.h
#ifdef A0
#undef A0
#endif // A0
#ifdef A1
#undef A1
#endif // A1
#ifdef A2
#undef A2
#endif // A2

/* Includes ---------------------------------------------------------------- */
#include "edge-impulse-sdk/classifier/ei_run_classifier.h"
#include "edge-impulse-sdk/classifier/ei_classifier_smooth.h"

#endif // _INFERENCE_H

#include "wrapper.h"

#ifdef __cplusplus
extern "C"
#endif

    EI_IMPULSE_ERROR
    wake_run_classifier(get_data callback, ei_impulse_result_t *result)
{
    signal_t signal;
    signal.total_length = EI_CLASSIFIER_RAW_SAMPLE_COUNT;
    signal.get_data = callback;
    EI_IMPULSE_ERROR r = run_classifier(&signal, result, false);
    return r;
}

#ifdef __cplusplus

#endif
