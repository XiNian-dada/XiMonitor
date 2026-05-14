/// 一个很小的 QR Code SVG 生成器,专门用于 TOTP 绑定页。
///
/// 这里固定使用 QR Model 2 / Version 5 / Error Correction L / Byte mode,
/// 容量足够容纳 XiMonitor 的 `otpauth://` URI。项目不把 TOTP secret 发给
/// 第三方二维码服务,所以需要在本地生成可扫码的 SVG。
const VERSION: usize = 5;
const SIZE: usize = 17 + VERSION * 4;
const DATA_CODEWORDS: usize = 108;
const ECC_CODEWORDS: usize = 26;
const MAX_BYTE_PAYLOAD: usize = 106;
const FORMAT_XOR_MASK: u16 = 0x5412;
const FORMAT_GENERATOR: u16 = 0x0537;

pub(crate) fn qr_svg_for_text(text: &str) -> anyhow::Result<String> {
    let bytes = text.as_bytes();
    if bytes.len() > MAX_BYTE_PAYLOAD {
        anyhow::bail!("QR payload is too long");
    }

    let mut data = encode_byte_mode(bytes);
    let ecc = reed_solomon_remainder(&data, ECC_CODEWORDS);
    data.extend(ecc);

    let mut matrix = QrMatrix::new();
    matrix.draw_function_patterns();
    matrix.draw_codewords(&data);
    matrix.apply_mask_0();
    matrix.draw_format_bits();
    Ok(matrix.to_svg())
}

fn encode_byte_mode(bytes: &[u8]) -> Vec<u8> {
    let mut bits = Vec::new();
    append_bits(&mut bits, 0b0100, 4);
    append_bits(&mut bits, bytes.len() as u32, 8);
    for byte in bytes {
        append_bits(&mut bits, u32::from(*byte), 8);
    }
    let capacity_bits = DATA_CODEWORDS * 8;
    let terminator = 4.min(capacity_bits.saturating_sub(bits.len()));
    bits.extend(std::iter::repeat_n(false, terminator));
    while bits.len() % 8 != 0 {
        bits.push(false);
    }

    let mut output = bits
        .chunks(8)
        .map(|chunk| {
            chunk
                .iter()
                .fold(0_u8, |acc, bit| (acc << 1) | u8::from(*bit))
        })
        .collect::<Vec<_>>();
    for pad in [0xec_u8, 0x11].iter().cycle() {
        if output.len() == DATA_CODEWORDS {
            break;
        }
        output.push(*pad);
    }
    output
}

fn append_bits(bits: &mut Vec<bool>, value: u32, count: usize) {
    for index in (0..count).rev() {
        bits.push(((value >> index) & 1) != 0);
    }
}

fn reed_solomon_remainder(data: &[u8], degree: usize) -> Vec<u8> {
    let generator = reed_solomon_generator(degree);
    let mut result = vec![0_u8; degree];
    for byte in data {
        let factor = byte ^ result[0];
        result.rotate_left(1);
        result[degree - 1] = 0;
        for (index, coefficient) in generator.iter().enumerate() {
            result[index] ^= gf_multiply(*coefficient, factor);
        }
    }
    result
}

fn reed_solomon_generator(degree: usize) -> Vec<u8> {
    let mut result = vec![1_u8];
    for index in 0..degree {
        let root = gf_pow(index);
        let mut next = vec![0_u8; result.len() + 1];
        for (coefficient_index, coefficient) in result.iter().enumerate() {
            next[coefficient_index] ^= gf_multiply(*coefficient, root);
            next[coefficient_index + 1] ^= *coefficient;
        }
        result = next;
    }
    result.remove(0);
    result
}

fn gf_pow(power: usize) -> u8 {
    let mut value = 1_u8;
    for _ in 0..power {
        value = gf_multiply(value, 2);
    }
    value
}

fn gf_multiply(mut left: u8, mut right: u8) -> u8 {
    let mut result = 0_u8;
    while right != 0 {
        if (right & 1) != 0 {
            result ^= left;
        }
        let carry = (left & 0x80) != 0;
        left <<= 1;
        if carry {
            left ^= 0x1d;
        }
        right >>= 1;
    }
    result
}

struct QrMatrix {
    modules: Vec<bool>,
    function: Vec<bool>,
}

impl QrMatrix {
    fn new() -> Self {
        Self {
            modules: vec![false; SIZE * SIZE],
            function: vec![false; SIZE * SIZE],
        }
    }

    fn draw_function_patterns(&mut self) {
        self.draw_finder(0, 0);
        self.draw_finder(SIZE - 7, 0);
        self.draw_finder(0, SIZE - 7);
        self.draw_timing_patterns();
        self.draw_alignment(30, 30);
        self.set_function(8, VERSION * 4 + 9, true);
        self.reserve_format_areas();
    }

    fn draw_finder(&mut self, x: usize, y: usize) {
        for dy in 0..8 {
            for dx in 0..8 {
                let xx = x + dx;
                let yy = y + dy;
                if xx >= SIZE || yy >= SIZE {
                    continue;
                }
                let dark = dx < 7
                    && dy < 7
                    && (dx == 0
                        || dx == 6
                        || dy == 0
                        || dy == 6
                        || ((2..=4).contains(&dx) && (2..=4).contains(&dy)));
                self.set_function(xx, yy, dark);
            }
        }
    }

    fn draw_timing_patterns(&mut self) {
        for index in 8..(SIZE - 8) {
            let dark = index % 2 == 0;
            self.set_function(6, index, dark);
            self.set_function(index, 6, dark);
        }
    }

    fn draw_alignment(&mut self, center_x: usize, center_y: usize) {
        for dy in 0..5 {
            for dx in 0..5 {
                let xx = center_x + dx - 2;
                let yy = center_y + dy - 2;
                let dark = dx == 0 || dx == 4 || dy == 0 || dy == 4 || (dx == 2 && dy == 2);
                self.set_function(xx, yy, dark);
            }
        }
    }

    fn reserve_format_areas(&mut self) {
        for index in 0..9 {
            if index != 6 {
                self.set_function(8, index, false);
                self.set_function(index, 8, false);
            }
        }
        for index in 0..8 {
            self.set_function(SIZE - 1 - index, 8, false);
            self.set_function(8, SIZE - 1 - index, false);
        }
    }

    fn draw_codewords(&mut self, codewords: &[u8]) {
        let bits = codewords
            .iter()
            .flat_map(|byte| (0..8).rev().map(move |index| ((byte >> index) & 1) != 0))
            .collect::<Vec<_>>();
        let mut bit_index = 0;
        let mut upward = true;
        let mut right = SIZE - 1;

        while right > 0 {
            if right == 6 {
                right -= 1;
            }
            for offset in 0..SIZE {
                let y = if upward { SIZE - 1 - offset } else { offset };
                for x in [right, right - 1] {
                    if self.is_function(x, y) {
                        continue;
                    }
                    self.set_module(x, y, bits.get(bit_index).copied().unwrap_or(false));
                    bit_index += 1;
                }
            }
            upward = !upward;
            if right < 2 {
                break;
            }
            right -= 2;
        }
    }

    fn apply_mask_0(&mut self) {
        for y in 0..SIZE {
            for x in 0..SIZE {
                if !self.is_function(x, y) && (x + y) % 2 == 0 {
                    let index = y * SIZE + x;
                    self.modules[index] = !self.modules[index];
                }
            }
        }
    }

    fn draw_format_bits(&mut self) {
        let bits = format_bits();
        for index in 0..6 {
            self.set_function(8, index, get_bit(bits, index));
        }
        self.set_function(8, 7, get_bit(bits, 6));
        self.set_function(8, 8, get_bit(bits, 7));
        self.set_function(7, 8, get_bit(bits, 8));
        for index in 9..15 {
            self.set_function(14 - index, 8, get_bit(bits, index));
        }
        for index in 0..8 {
            self.set_function(SIZE - 1 - index, 8, get_bit(bits, index));
        }
        for index in 8..15 {
            self.set_function(8, SIZE - 15 + index, get_bit(bits, index));
        }
        self.set_function(8, SIZE - 8, true);
    }

    fn to_svg(&self) -> String {
        let quiet = 4;
        let view_size = SIZE + quiet * 2;
        let mut svg = format!(
            r##"<svg class="totp-qr" viewBox="0 0 {view_size} {view_size}" role="img" aria-label="TOTP QR code" shape-rendering="crispEdges" xmlns="http://www.w3.org/2000/svg"><rect width="{view_size}" height="{view_size}" rx="2" fill="#fff"/>"##
        );
        for y in 0..SIZE {
            for x in 0..SIZE {
                if self.module(x, y) {
                    svg.push_str(&format!(
                        r##"<rect x="{}" y="{}" width="1" height="1" fill="#0f172a"/>"##,
                        x + quiet,
                        y + quiet
                    ));
                }
            }
        }
        svg.push_str("</svg>");
        svg
    }

    fn set_function(&mut self, x: usize, y: usize, dark: bool) {
        self.set_module(x, y, dark);
        self.function[y * SIZE + x] = true;
    }

    fn set_module(&mut self, x: usize, y: usize, dark: bool) {
        self.modules[y * SIZE + x] = dark;
    }

    fn module(&self, x: usize, y: usize) -> bool {
        self.modules[y * SIZE + x]
    }

    fn is_function(&self, x: usize, y: usize) -> bool {
        self.function[y * SIZE + x]
    }
}

fn format_bits() -> u16 {
    // Error correction L = 01, mask pattern 000.
    let data = 0b01_u16 << 3;
    let mut remainder = data << 10;
    for shift in (0..=4).rev() {
        if ((remainder >> (shift + 10)) & 1) != 0 {
            remainder ^= FORMAT_GENERATOR << shift;
        }
    }
    ((data << 10) | (remainder & 0x03ff)) ^ FORMAT_XOR_MASK
}

fn get_bit(value: u16, index: usize) -> bool {
    ((value >> index) & 1) != 0
}

#[cfg(test)]
mod tests {
    use super::qr_svg_for_text;

    #[test]
    fn renders_totp_uri_as_inline_svg() {
        let svg = qr_svg_for_text(
            "otpauth://totp/XiMonitor:viewer?secret=JBSWY3DPEHPK3PXP&issuer=XiMonitor",
        )
        .expect("sample otpauth URI should fit in version 5-L");

        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("viewBox=\"0 0 45 45\""));
        assert!(svg.contains("<rect"));
    }

    #[test]
    fn rejects_payloads_that_do_not_fit_fixed_qr_version() {
        let payload = "x".repeat(107);
        assert!(qr_svg_for_text(&payload).is_err());
    }
}
