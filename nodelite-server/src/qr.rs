/// TOTP 绑定页使用的 QR Code SVG 生成器。
///
/// 这里依赖成熟的 `qrcode` crate,避免维护自研 QR 编码、纠错和掩码逻辑。
use qrcode::{EcLevel, QrCode, render::svg};
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum QrError {
    #[error("failed to encode TOTP QR code")]
    Encode(#[from] qrcode::types::QrError),
}

pub(crate) fn qr_svg_for_text(text: &str) -> Result<String, QrError> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M)?;
    let svg = code
        .render::<svg::Color<'_>>()
        .min_dimensions(320, 320)
        .quiet_zone(true)
        .dark_color(svg::Color("#0f172a"))
        .light_color(svg::Color("#ffffff"))
        .build();

    Ok(add_accessibility_attributes(svg))
}

fn add_accessibility_attributes(svg: String) -> String {
    svg.replacen(
        "<svg ",
        r#"<svg class="totp-qr" role="img" aria-label="TOTP QR code" shape-rendering="crispEdges" "#,
        1,
    )
}

#[cfg(test)]
mod tests {
    use super::qr_svg_for_text;

    #[test]
    fn renders_totp_uri_as_inline_svg() {
        let svg = qr_svg_for_text(
            "otpauth://totp/NodeLite:viewer?secret=JBSWY3DPEHPK3PXP&issuer=NodeLite",
        )
        .expect("sample otpauth URI should render");

        assert!(svg.contains("<svg"));
        assert!(svg.contains("class=\"totp-qr\""));
        assert!(svg.contains("viewBox="));
        assert!(svg.contains("TOTP QR code"));
        assert!(svg.contains("<path"));
    }

    #[test]
    fn rejects_payloads_that_do_not_fit() {
        let payload = "x".repeat(8192);
        assert!(qr_svg_for_text(&payload).is_err());
    }
}
