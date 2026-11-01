//! Minimal reader for little-endian float32 C-order `.npy` files.

const MAGIC: &[u8] = b"\x93NUMPY";

/// Values and shape of a `<f4` array.
pub fn read_f32(bytes: &[u8]) -> Result<(Vec<f32>, Vec<usize>), String> {
    let rest = bytes.strip_prefix(MAGIC).ok_or("not an .npy file")?;
    let (header_len, offset) = match rest.first() {
        Some(1) => (usize::from(u16::from_le_bytes([rest[2], rest[3]])), 4),
        Some(2 | 3) if rest.len() >= 6 => (
            u32::from_le_bytes([rest[2], rest[3], rest[4], rest[5]]) as usize,
            6,
        ),
        _ => return Err("unsupported .npy version".into()),
    };
    let start = MAGIC.len() + offset;
    let header = bytes
        .get(start..start + header_len)
        .and_then(|h| std::str::from_utf8(h).ok())
        .ok_or("truncated .npy header")?;
    if !header.contains("'descr': '<f4'") || !header.contains("'fortran_order': False") {
        return Err(format!(
            ".npy is not little-endian float32 in C order: {header}"
        ));
    }
    let shape_text = header
        .split("'shape': (")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .ok_or("no shape in .npy header")?;
    let shape = shape_text
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<usize>().map_err(|e| format!(".npy shape: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    let data = &bytes[start + header_len..];
    let count: usize = shape.iter().product();
    if data.len() != count * 4 {
        return Err(format!(
            ".npy holds {} bytes for shape {shape:?}",
            data.len()
        ));
    }
    let values = data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    Ok((values, shape))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn npy(header: &str, values: &[f32]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&[1, 0]);
        out.extend_from_slice(&(header.len() as u16).to_le_bytes());
        out.extend_from_slice(header.as_bytes());
        for v in values {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    #[test]
    fn reads_shape_and_values() {
        let bytes = npy(
            "{'descr': '<f4', 'fortran_order': False, 'shape': (1, 2, 3), }\n",
            &[0.5; 6],
        );
        let (values, shape) = read_f32(&bytes).unwrap();
        assert_eq!(shape, [1, 2, 3]);
        assert_eq!(values, [0.5; 6]);
    }

    #[test]
    fn rejects_other_dtypes_and_short_data() {
        let f8 = npy(
            "{'descr': '<f8', 'fortran_order': False, 'shape': (1,), }\n",
            &[0.0; 2],
        );
        assert!(read_f32(&f8).is_err());
        let short = npy(
            "{'descr': '<f4', 'fortran_order': False, 'shape': (3,), }\n",
            &[0.0; 2],
        );
        assert!(read_f32(&short).is_err());
    }
}
