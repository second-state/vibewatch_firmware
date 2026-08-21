pub const BACKGROUND_GIF_KEY: &str = "bg_gif";
pub const MAX_BACKGROUND_GIF_BYTES: usize = 128 * 1024;

pub fn validate_gif(data: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(!data.is_empty(), "GIF data is empty");
    anyhow::ensure!(
        data.len() <= MAX_BACKGROUND_GIF_BYTES,
        "GIF data exceeds {}KB",
        MAX_BACKGROUND_GIF_BYTES / 1024
    );
    anyhow::ensure!(
        data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a"),
        "background must be a GIF"
    );
    tinygif::Gif::<crate::ui::UiColor>::from_slice(data)
        .map_err(|e| anyhow::anyhow!("invalid GIF data: {e:?}"))?;
    Ok(())
}

pub fn load_from_nvs(nvs: &esp_idf_svc::nvs::EspDefaultNvs) -> Option<&'static [u8]> {
    let len = nvs.blob_len(BACKGROUND_GIF_KEY).ok()??;
    if len == 0 {
        return None;
    }
    if len > MAX_BACKGROUND_GIF_BYTES {
        log::error!(
            "Stored background GIF too large: {}B > {}B",
            len,
            MAX_BACKGROUND_GIF_BYTES
        );
        return None;
    }

    let mut data = vec![0u8; len];
    let data = nvs.get_blob(BACKGROUND_GIF_KEY, &mut data).ok()??;
    if let Err(e) = validate_gif(data) {
        log::error!("Stored background GIF is invalid: {e:?}");
        return None;
    }
    log::info!("Loaded custom background GIF: {}B", data.len());
    Some(Box::leak(data.to_vec().into_boxed_slice()))
}

pub fn save_to_nvs(nvs: &esp_idf_svc::nvs::EspDefaultNvs, data: &[u8]) -> anyhow::Result<()> {
    validate_gif(data)?;
    nvs.set_blob(BACKGROUND_GIF_KEY, data)?;
    Ok(())
}
