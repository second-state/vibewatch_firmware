use esp_idf_svc::sys;
use std::io::Write;

#[derive(Debug, Clone)]
pub struct WavConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
}

impl Default for WavConfig {
    fn default() -> Self {
        Self {
            sample_rate: crate::audio::SAMPLE_RATE,
            channels: 1,
            bits_per_sample: 16,
        }
    }
}

pub fn create_unlimited_wav_header(config: &WavConfig) -> Vec<u8> {
    let mut wav_data = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut wav_data);

    let bytes_per_sample = config.bits_per_sample / 8;
    let byte_rate = config.sample_rate * config.channels as u32 * bytes_per_sample as u32;
    let block_align = config.channels * bytes_per_sample;
    let data_size = 0xFFFFFFFFu32;
    let file_size = 0x7FFFFFFFu32;

    cursor.write_all(b"RIFF").unwrap();
    cursor.write_all(&file_size.to_le_bytes()).unwrap();
    cursor.write_all(b"WAVE").unwrap();
    cursor.write_all(b"fmt ").unwrap();
    cursor.write_all(&16u32.to_le_bytes()).unwrap();
    cursor.write_all(&1u16.to_le_bytes()).unwrap();
    cursor.write_all(&config.channels.to_le_bytes()).unwrap();
    cursor.write_all(&config.sample_rate.to_le_bytes()).unwrap();
    cursor.write_all(&byte_rate.to_le_bytes()).unwrap();
    cursor.write_all(&block_align.to_le_bytes()).unwrap();
    cursor
        .write_all(&config.bits_per_sample.to_le_bytes())
        .unwrap();
    cursor.write_all(b"data").unwrap();
    cursor.write_all(&data_size.to_le_bytes()).unwrap();

    wav_data
}

/// Logs current free heap (total / internal / PSRAM) with a context tag.
pub fn log_heap_usage(tag: &str) {
    use esp_idf_svc::sys;
    // SAFETY: plain ESP-IDF heap queries.
    let (free, min, internal, psram) = unsafe {
        (
            sys::esp_get_free_heap_size(),
            sys::esp_get_minimum_free_heap_size(),
            sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL),
            sys::heap_caps_get_free_size(sys::MALLOC_CAP_SPIRAM),
        )
    };
    log::info!(
        "[heap] {tag}: free {} KB, min {} KB, internal {} KB, psram {} KB",
        free / 1024,
        min / 1024,
        internal / 1024,
        psram / 1024
    );
}

// --- PSRAM-stack task wrapper ------------------------------------------------

/// A FreeRTOS task whose stack is allocated from PSRAM (the TCB stays in
/// internal RAM). Rust-side counterpart of the ml_create_task_psram() patch
/// in the microlink fork. Not used yet.
#[allow(dead_code)]
pub struct PsramTask {
    handle: sys::TaskHandle_t,
    stack: *mut sys::StackType_t,
    tcb: *mut sys::StaticTask_t,
}

#[allow(dead_code)]
impl PsramTask {
    /// Spawns `task_fn(arg)` pinned to `core`, with a PSRAM stack of
    /// `stack_size` **bytes** (ESP-IDF measures depth in bytes, unlike
    /// vanilla FreeRTOS). Requires CONFIG_SPIRAM_ALLOW_STACK_EXTERNAL_MEMORY.
    ///
    /// # Safety
    /// - `task_fn` and `arg` must stay valid for as long as the task runs.
    /// - The task must have stopped before this value is dropped (its stack
    ///   is freed by [`Drop`]).
    pub unsafe fn spawn(
        name: &core::ffi::CStr,
        task_fn: sys::TaskFunction_t,
        arg: *mut core::ffi::c_void,
        stack_size: usize,
        priority: sys::UBaseType_t,
        core: sys::BaseType_t,
    ) -> anyhow::Result<Self> {
        let tcb = sys::heap_caps_malloc(
            core::mem::size_of::<sys::StaticTask_t>(),
            sys::MALLOC_CAP_INTERNAL | sys::MALLOC_CAP_8BIT,
        );
        if tcb.is_null() {
            anyhow::bail!("PSRAM task {name:?}: TCB alloc failed");
        }
        let stack =
            sys::heap_caps_malloc(stack_size, sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_8BIT);
        if stack.is_null() {
            sys::heap_caps_free(tcb);
            anyhow::bail!("PSRAM task {name:?}: stack alloc failed");
        }
        let handle = sys::xTaskCreateStaticPinnedToCore(
            task_fn,
            name.as_ptr(),
            stack_size as u32,
            arg,
            priority,
            stack.cast(),
            tcb.cast(),
            core,
        );
        if handle.is_null() {
            sys::heap_caps_free(stack);
            sys::heap_caps_free(tcb);
            anyhow::bail!("PSRAM task {name:?}: xTaskCreateStaticPinnedToCore failed");
        }
        log::info!("PSRAM task {name:?} spawned ({} B PSRAM stack)", stack_size);
        Ok(Self {
            handle,
            stack: stack.cast(),
            tcb: tcb.cast(),
        })
    }

    pub fn handle(&self) -> sys::TaskHandle_t {
        self.handle
    }
}

#[allow(dead_code)]
impl Drop for PsramTask {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful heap_caps_malloc calls in spawn.
        unsafe {
            if !self.handle.is_null() {
                sys::vTaskDelete(self.handle);
            }
            sys::heap_caps_free(self.stack.cast());
            sys::heap_caps_free(self.tcb.cast());
        }
    }
}
