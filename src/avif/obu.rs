//! AV1 OBU stream handling for AVIF items: drop temporal delimiters and
//! padding (not allowed in item data) and build `av1C` from the sequence
//! header.

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_PADDING: u8 = 15;

/// Item payload plus its `av1C` box body.
pub struct Av1Item {
    pub data: Vec<u8>,
    pub av1c: Vec<u8>,
    /// `seq_level_idx[0]`, for the AVIF baseline brand check.
    pub level: u8,
}

fn leb128(b: &[u8], mut i: usize) -> Option<(usize, usize)> {
    let mut v = 0usize;
    for k in 0..8 {
        let x = *b.get(i)?;
        i += 1;
        v |= usize::from(x & 0x7f) << (7 * k);
        if x & 0x80 == 0 {
            return Some((v, i));
        }
    }
    None
}

/// Split the encoder output into item data and `av1C`.
pub fn to_item(stream: &[u8]) -> Result<Av1Item, String> {
    let mut data = Vec::with_capacity(stream.len());
    let mut seq: Option<SequenceHeader> = None;
    let mut seq_obu: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < stream.len() {
        let header = stream[i];
        let kind = (header >> 3) & 0x0f;
        let ext = usize::from((header >> 2) & 1);
        if header & 0x02 == 0 {
            return Err("AV1 stream: OBU without size field".into());
        }
        let (size, payload) =
            leb128(stream, i + 1 + ext).ok_or("AV1 stream: truncated OBU header")?;
        let end = payload
            .checked_add(size)
            .filter(|&e| e <= stream.len())
            .ok_or("AV1 stream: truncated OBU")?;
        match kind {
            OBU_TEMPORAL_DELIMITER | OBU_PADDING => {}
            _ => {
                if kind == OBU_SEQUENCE_HEADER && seq.is_none() {
                    seq = Some(SequenceHeader::parse(&stream[payload..end])?);
                    seq_obu = stream[i..end].to_vec();
                }
                data.extend_from_slice(&stream[i..end]);
            }
        }
        i = end;
    }
    let seq = seq.ok_or("AV1 stream: no sequence header")?;
    Ok(Av1Item {
        data,
        av1c: seq.av1c(&seq_obu),
        level: seq.level,
    })
}

#[derive(Debug, PartialEq)]
pub struct SequenceHeader {
    pub profile: u8,
    pub level: u8,
    pub tier: u8,
    pub bit_depth: u8,
    pub mono: bool,
    pub subsampling: (u8, u8),
    pub chroma_position: u8,
    pub cicp: (u8, u8, u8),
    pub full_range: bool,
    pub film_grain: bool,
}

struct Bits<'a> {
    b: &'a [u8],
    p: usize,
}

impl Bits<'_> {
    fn f(&mut self, n: usize) -> Result<u32, String> {
        let mut v = 0;
        for _ in 0..n {
            let byte = *self
                .b
                .get(self.p >> 3)
                .ok_or("AV1 sequence header: truncated")?;
            v = (v << 1) | u32::from((byte >> (7 - (self.p & 7))) & 1);
            self.p += 1;
        }
        Ok(v)
    }

    fn flag(&mut self) -> Result<bool, String> {
        Ok(self.f(1)? == 1)
    }
}

impl SequenceHeader {
    pub fn parse(b: &[u8]) -> Result<Self, String> {
        let mut r = Bits { b, p: 0 };
        let profile = r.f(3)? as u8;
        let _still = r.f(1)?;
        let reduced = r.flag()?;
        let (level, tier);
        if reduced {
            level = r.f(5)? as u8;
            tier = 0;
        } else {
            if r.flag()? {
                return Err("AV1 sequence header: timing info not supported".into());
            }
            let delay_present = r.flag()?;
            let count = r.f(5)? + 1;
            let (mut first_level, mut first_tier) = (0, 0);
            for k in 0..count {
                r.f(12)?;
                let l = r.f(5)? as u8;
                let t = if l > 7 { r.f(1)? as u8 } else { 0 };
                if k == 0 {
                    (first_level, first_tier) = (l, t);
                }
                if delay_present && r.flag()? {
                    r.f(4)?;
                }
            }
            (level, tier) = (first_level, first_tier);
        }
        let wb = r.f(4)? as usize + 1;
        let hb = r.f(4)? as usize + 1;
        r.f(wb)?;
        r.f(hb)?;
        if !reduced && r.flag()? {
            r.f(7)?;
        }
        r.f(3)?; // 128x128 SB, filter intra, intra edge
        if !reduced {
            r.f(4)?; // interintra, masked, warped, dual filter
            let order_hint = r.flag()?;
            if order_hint {
                r.f(2)?;
            }
            let force_sct = if r.flag()? { 2 } else { r.f(1)? };
            if force_sct > 0 && !r.flag()? {
                r.f(1)?;
            }
            if order_hint {
                r.f(3)?;
            }
        }
        r.f(3)?; // superres, cdef, restoration
        let high = r.flag()?;
        let bit_depth = if profile == 2 && high {
            if r.flag()? { 12 } else { 10 }
        } else if high {
            10
        } else {
            8
        };
        let mono = if profile == 1 { false } else { r.flag()? };
        let cicp = if r.flag()? {
            (r.f(8)? as u8, r.f(8)? as u8, r.f(8)? as u8)
        } else {
            (2, 2, 2)
        };
        let (full_range, subsampling, chroma_position);
        if mono {
            full_range = r.flag()?;
            (subsampling, chroma_position) = ((1, 1), 0);
        } else if cicp == (1, 13, 0) {
            full_range = true;
            (subsampling, chroma_position) = ((0, 0), 0);
        } else {
            full_range = r.flag()?;
            let (sx, sy) = match profile {
                0 => (1, 1),
                1 => (0, 0),
                _ if bit_depth == 12 => {
                    let sx = r.f(1)? as u8;
                    (sx, if sx == 1 { r.f(1)? as u8 } else { 0 })
                }
                _ => (1, 0),
            };
            chroma_position = if sx == 1 && sy == 1 { r.f(2)? as u8 } else { 0 };
            subsampling = (sx, sy);
        }
        if !mono {
            r.f(1)?; // separate_uv_delta_q
        }
        let film_grain = r.flag()?;
        Ok(Self {
            profile,
            level,
            tier,
            bit_depth,
            mono,
            subsampling,
            chroma_position,
            cicp,
            full_range,
            film_grain,
        })
    }

    /// `av1C` body (AV1-ISOBMFF 2.3.3) with the sequence header OBU.
    fn av1c(&self, seq_obu: &[u8]) -> Vec<u8> {
        let mut v = vec![
            0x81,
            (self.profile << 5) | (self.level & 0x1f),
            (self.tier << 7)
                | (u8::from(self.bit_depth > 8) << 6)
                | (u8::from(self.bit_depth == 12) << 5)
                | (u8::from(self.mono) << 4)
                | (self.subsampling.0 << 3)
                | (self.subsampling.1 << 2)
                | self.chroma_position,
            0,
        ];
        v.extend_from_slice(seq_obu);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_svt_stream_round_trips() {
        let (w, h) = (66u32, 34u32);
        let y = vec![300u16; (w * h) as usize];
        let c = vec![512u16; (w.div_ceil(2) * h.div_ceil(2)) as usize];
        let params = [
            ("avif", "1"),
            ("preset", "12"),
            ("color-primaries", "12"),
            ("transfer-characteristics", "13"),
            ("matrix-coefficients", "6"),
            ("color-range", "1"),
        ];
        let stream = svt_av1_tritium_sys::encode_yuv420_10(
            &params,
            None,
            &svt_av1_tritium_sys::Yuv420 {
                width: w,
                height: h,
                y: &y,
                u: &c,
                v: &c,
            },
        )
        .unwrap();
        let item = to_item(&stream).unwrap();
        assert!(item.data.len() <= stream.len());
        assert_eq!(item.av1c[0], 0x81);
        let seq_payload = &item.av1c[4 + 2..]; // OBU header + 1-byte size
        let seq = SequenceHeader::parse(seq_payload).unwrap();
        assert_eq!(
            (
                seq.profile,
                seq.bit_depth,
                seq.subsampling,
                seq.cicp,
                seq.full_range
            ),
            (0, 10, (1, 1), (12, 13, 6), true)
        );
    }
}
