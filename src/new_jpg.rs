use esp_idf_svc::sys::*;

struct JpegDecoder {
    handle: jpeg_dec_handle_t,
}

impl JpegDecoder {
    fn open(config: &jpeg_dec_config_t) -> Result<Self, i32> {
        unsafe {
            let mut handle: jpeg_dec_handle_t = std::ptr::null_mut();
            let ret = jpeg_dec_open(
                config as *const jpeg_dec_config_t as *mut jpeg_dec_config_t,
                &mut handle,
            );
            if ret != jpeg_error_t_JPEG_ERR_OK {
                return Err(ret);
            }
            Ok(JpegDecoder { handle })
        }
    }
}

impl Drop for JpegDecoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                jpeg_dec_close(self.handle);
            }
        }
    }
}

pub struct JpegBufferu16 {
    pub data: Vec<u128>,
    pub width: usize,
    pub height: usize,
}

impl JpegBufferu16 {
    pub fn new(width: usize, height: usize) -> Self {
        let data = vec![0u128; width * height * 2 / 16]; // 2 bytes per pixel for RGB565

        JpegBufferu16 {
            data,
            width,
            height,
        }
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.data.as_mut_ptr() as *mut u8
    }

    /// 把整张图刷到 LCD (0, 0, width, height)。412×412 单页用这个。
    pub fn flush_to_lcd(&self) -> anyhow::Result<()> {
        let ptr = unsafe {
            std::slice::from_raw_parts(self.data.as_ptr() as *const u8, self.data.len() * 16)
        };

        let e = crate::lcd::flush_display(ptr, 0, 0, self.width as i32, self.height as i32);
        if e != 0 {
            Err(anyhow::anyhow!("Failed to flush to LCD: error code {}", e))
        } else {
            Ok(())
        }
    }

    /// 取缓冲区 `[offset, offset+win_h)` 像素行刷到 LCD(本地滚动用,目前未用,保留备用)。
    pub fn flush_window(&self, offset: usize, win_h: usize) -> anyhow::Result<()> {
        let ptr = unsafe {
            std::slice::from_raw_parts(self.data.as_ptr() as *const u8, self.data.len() * 16)
        };

        let max_offset = self.height.saturating_sub(win_h);
        let off = offset.min(max_offset);
        let size = (self.height - off).min(win_h);
        let start = off * self.width * 2; // 2 bytes per pixel for RGB565
        let end = start + size * self.width * 2;

        let e = crate::lcd::flush_display(&ptr[start..end], 0, 0, self.width as i32, size as i32);
        if e != 0 {
            Err(anyhow::anyhow!("Failed to flush to LCD: error code {}", e))
        } else {
            Ok(())
        }
    }
}

/// 解码一帧 JPEG 到 RGB565 缓冲。
///
/// `config.scale` 决定硬解码器的输出分辨率。手表屏幕 412×412,这里设成 412×412
/// 让服务端按 Sync{412,412} 渲染的整屏图直接满屏。
///
/// 注意:412 不是 8 的倍数,若运行时 `jpeg_dec` 拒绝该 scale(解码报错/花屏),
/// 把下面的 width/height 以及 `protocol::ClientMessage::sync()` 一起改成 **408**(8×51)。
pub fn esp_jpeg_decode_one_picture(data: &[u8]) -> anyhow::Result<JpegBufferu16> {
    unsafe {
        use esp_idf_svc::sys::*;

        // Generate default configuration
        let mut config = jpeg_dec_config_t::default();
        config.output_type = jpeg_pixel_format_t_JPEG_PIXEL_FORMAT_RGB565_LE;

        // 手表 412×412(若运行时不接受,改 408×408,并同步 protocol::ClientMessage::sync())
        config.scale.height = 412;
        config.scale.width = 412;

        // Create jpeg_dec handle
        let decoder = JpegDecoder::open(&config)
            .map_err(|e| anyhow::anyhow!("Failed to open JPEG decoder: error code {}", e))?;

        // Create io_callback handle
        let mut jpeg_io = Box::new(jpeg_dec_io_t::default());

        // Create out_info handle
        let mut out_info = Box::new(jpeg_dec_header_info_t::default());

        // Set input buffer and buffer len to io_callback
        jpeg_io.inbuf = data.as_ptr() as *mut u8;
        jpeg_io.inbuf_len = data.len() as i32;

        // Parse jpeg picture header and get picture for user and decoder
        let ret = jpeg_dec_parse_header(decoder.handle, jpeg_io.as_mut(), out_info.as_mut());
        if ret != jpeg_error_t_JPEG_ERR_OK {
            return Err(anyhow::anyhow!(
                "Failed to parse JPEG header: error code {}",
                ret
            ));
        }

        // Allocate output buffer sized to the decoder's reported output dimensions.
        let mut out_buf =
            JpegBufferu16::new((*out_info).width as usize, (*out_info).height as usize);

        jpeg_io.outbuf = out_buf.as_mut_ptr() as *mut u8;

        // Start decode jpeg
        let ret = jpeg_dec_process(decoder.handle, jpeg_io.as_mut());
        if ret != jpeg_error_t_JPEG_ERR_OK {
            return Err(anyhow::anyhow!("Failed to decode JPEG: error code {}", ret));
        }

        Ok(out_buf)
    }
}
