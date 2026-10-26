//! Loading-time WGSL variants for WebGPU's supported one/four sample modes.
use std::borrow::Cow;

pub(crate) fn shader(source: &str, samples: u32) -> Cow<'_, str> {
    assert!(matches!(samples, 1 | 4));
    if samples == 4 {
        return Cow::Borrowed(source);
    }
    // The third textureLoad argument becomes mip zero: every fragment in the
    // single-sample variant has sample_index zero. Only sample reduction loops
    // change; four-corner bilinear interpolation and quad topology do not.
    Cow::Owned(
        source
            .replace("texture_depth_multisampled_2d", "texture_depth_2d")
            .replace("texture_multisampled_2d", "texture_2d")
            .replace("sample<4u", "sample<1u")
            .replace("sample<4;", "sample<1;")
            .replace("i<4;i++", "i<1;i++")
            .replace("i)*0.25", "i)*1.0"),
    )
}

pub(crate) fn color_bytes_per_pixel(samples: u32) -> u64 {
    if samples == 1 {
        8
    } else {
        u64::from(samples + 1) * 8
    }
}
