
#include "edge-impulse-sdk/dsp/returntypes.h"
#include "edge-impulse-sdk/classifier/ei_classifier_types.h"
#include "model-parameters/model_metadata.h"

typedef int (*get_data)(size_t, size_t, float *);

#ifdef __cplusplus

extern "C"
{
#endif

    EI_IMPULSE_ERROR wake_run_classifier(get_data, ei_impulse_result_t *);

#ifdef __cplusplus
}
#endif