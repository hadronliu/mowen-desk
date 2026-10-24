//! 录音 PCM → WAV 封装（16 kHz 单声道 PCM16，与极趣 SDK 一致）。

/// 手写 44 字节标准 WAV 头 + PCM 数据，避免额外依赖。
pub fn pcm16_to_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let byte_rate = sample_rate * 2; // 单声道 16bit
    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    buf
}

/// 秒 → mm:ss
pub fn fmt_duration(samples: usize, sample_rate: u32) -> String {
    let secs = samples as u64 / sample_rate as u64;
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// WAV → M4A（AAC）转码。
/// 墨问上传 policy 的音频 MIME 白名单只有 mpeg/mp4/m4a 系列，不收 audio/wav。
/// 优先用系统 afconvert（macOS 自带）；返回 None 时调用方应降级为不上传音频。
pub fn transcode_to_m4a(wav_path: &std::path::Path) -> Option<Vec<u8>> {
    let out = wav_path.with_extension("m4a");
    let status = std::process::Command::new("afconvert")
        .args(["-f", "m4af", "-d", "aac"])
        .arg(wav_path)
        .arg(&out)
        .status()
        .ok()?;
    if !status.success() {
        eprintln!("[voice] afconvert 转码失败：{status}");
        let _ = std::fs::remove_file(&out);
        return None;
    }
    let bytes = std::fs::read(&out).ok();
    let _ = std::fs::remove_file(&out);
    bytes
}
