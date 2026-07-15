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
