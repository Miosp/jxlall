//! AVIF (HEIF) container writer, laid out like libavif's output: one color
//! item or grid, optional alpha (`auxl`), optional gain map (`tmap` derived
//! item in an `altr` group with the color item), Exif and XMP (`cdsc`).

/// One coded AV1 picture.
pub struct Coded {
    pub data: Vec<u8>,
    pub av1c: Vec<u8>,
    pub level: u8,
}

/// A displayable image: one coded picture, or a grid of equal tiles
/// (row-major) cropped to `width` x `height`.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub tiles: Vec<Coded>,
    pub cols: u32,
    pub rows: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    /// `pixi` channel count: 3 for color, 1 for alpha and gain maps.
    pub channels: u8,
    pub depth: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct Nclx {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

pub struct GainMap {
    pub image: Image,
    /// `tmap` item payload: version byte + ISO 21496-1 metadata.
    pub metadata: Vec<u8>,
}

pub struct File<'a> {
    pub color: Image,
    pub nclx: Nclx,
    pub icc: Option<&'a [u8]>,
    pub alpha: Option<Image>,
    pub gain_map: Option<GainMap>,
    pub exif: Option<&'a [u8]>,
    pub xmp: Option<&'a [u8]>,
}

const ALPHA_URN: &[u8] = b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0";
/// AVIF baseline profile allows AV1 levels up to 5.1.
const MAX_BASELINE_LEVEL: u8 = 13;

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 8);
    v.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(payload);
    v
}

fn full(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(payload.len() + 4);
    p.push(version);
    p.extend_from_slice(&flags.to_be_bytes()[1..]);
    p.extend_from_slice(payload);
    bx(kind, &p)
}

struct Item {
    id: u16,
    kind: [u8; 4],
    hidden: bool,
    content_type: Option<&'static str>,
    data: Vec<u8>,
    /// 1-based property index, essential flag.
    props: Vec<(usize, bool)>,
}

#[derive(Default)]
struct Builder {
    items: Vec<Item>,
    props: Vec<Vec<u8>>,
    refs: Vec<([u8; 4], u16, Vec<u16>)>,
}

impl Builder {
    fn prop(&mut self, bytes: Vec<u8>) -> usize {
        match self.props.iter().position(|p| *p == bytes) {
            Some(i) => i + 1,
            None => {
                self.props.push(bytes);
                self.props.len()
            }
        }
    }

    fn item(&mut self, kind: &[u8; 4], data: Vec<u8>) -> u16 {
        let id = self.items.len() as u16 + 1;
        self.items.push(Item {
            id,
            kind: *kind,
            hidden: false,
            content_type: None,
            data,
            props: Vec::new(),
        });
        id
    }

    fn get(&mut self, id: u16) -> &mut Item {
        &mut self.items[usize::from(id) - 1]
    }

    fn associate(&mut self, id: u16, bytes: Vec<u8>, essential: bool) {
        let p = self.prop(bytes);
        self.get(id).props.push((p, essential));
    }

    /// Adds `img`; returns the id of its displayable item.
    fn image(&mut self, img: Image, extra: &[(Vec<u8>, bool)], hidden: bool) -> u16 {
        let pixi = full(b"pixi", 0, 0, &{
            let mut p = vec![img.channels];
            p.extend(std::iter::repeat_n(img.depth, usize::from(img.channels)));
            p
        });
        let ispe = |w: u32, h: u32| {
            let mut p = w.to_be_bytes().to_vec();
            p.extend_from_slice(&h.to_be_bytes());
            full(b"ispe", 0, 0, &p)
        };
        let coded_item = |b: &mut Self, c: Coded, w: u32, h: u32| {
            let id = b.item(b"av01", c.data);
            b.associate(id, ispe(w, h), false);
            b.associate(id, pixi.clone(), false);
            b.associate(id, bx(b"av1C", &c.av1c), true);
            id
        };
        let top = if img.tiles.len() == 1 {
            let c = img.tiles.into_iter().next().expect("one tile");
            coded_item(self, c, img.width, img.height)
        } else {
            let large = img.width > u32::from(u16::MAX) || img.height > u32::from(u16::MAX);
            let mut desc = vec![
                0,
                u8::from(large),
                (img.rows - 1) as u8,
                (img.cols - 1) as u8,
            ];
            if large {
                desc.extend_from_slice(&img.width.to_be_bytes());
                desc.extend_from_slice(&img.height.to_be_bytes());
            } else {
                desc.extend_from_slice(&(img.width as u16).to_be_bytes());
                desc.extend_from_slice(&(img.height as u16).to_be_bytes());
            }
            let grid = self.item(b"grid", desc);
            let tiles: Vec<u16> = img
                .tiles
                .into_iter()
                .map(|c| {
                    let id = coded_item(self, c, img.tile_width, img.tile_height);
                    self.get(id).hidden = true;
                    id
                })
                .collect();
            self.refs.push((*b"dimg", grid, tiles));
            self.associate(grid, ispe(img.width, img.height), false);
            self.associate(grid, pixi, false);
            grid
        };
        for (p, essential) in extra {
            self.associate(top, p.clone(), *essential);
        }
        self.get(top).hidden = hidden;
        top
    }
}

fn nclx_box(n: Nclx) -> Vec<u8> {
    let mut p = b"nclx".to_vec();
    for v in [n.primaries, n.transfer, n.matrix] {
        p.extend_from_slice(&u16::from(v).to_be_bytes());
    }
    p.push(u8::from(n.full_range) << 7);
    bx(b"colr", &p)
}

fn levels(img: &Image) -> impl Iterator<Item = u8> + '_ {
    img.tiles.iter().map(|t| t.level)
}

pub fn write(f: File) -> Vec<u8> {
    let baseline = levels(&f.color)
        .chain(f.alpha.iter().flat_map(levels))
        .chain(f.gain_map.iter().flat_map(|g| levels(&g.image)))
        .all(|l| l <= MAX_BASELINE_LEVEL);
    let (width, height) = (f.color.width, f.color.height);
    let mut b = Builder::default();
    let mut color_props = Vec::new();
    if let Some(icc) = f.icc {
        let mut p = b"prof".to_vec();
        p.extend_from_slice(icc);
        color_props.push((bx(b"colr", &p), false));
    }
    color_props.push((nclx_box(f.nclx), false));
    let color = b.image(f.color, &color_props, false);
    if let Some(alpha) = f.alpha {
        let id = b.image(alpha, &[(full(b"auxC", 0, 0, ALPHA_URN), true)], false);
        b.refs.push((*b"auxl", id, vec![color]));
    }
    let mut altr = None;
    let has_gain_map = f.gain_map.is_some();
    if let Some(gm) = f.gain_map {
        let tmap = b.item(b"tmap", gm.metadata);
        let mut ispe = width.to_be_bytes().to_vec();
        ispe.extend_from_slice(&height.to_be_bytes());
        b.associate(tmap, full(b"ispe", 0, 0, &ispe), false);
        b.associate(tmap, nclx_box(f.nclx), false);
        let map = b.image(gm.image, &[], true);
        b.refs.push((*b"dimg", tmap, vec![color, map]));
        altr = Some(vec![tmap, color]);
    }
    for (payload, kind, content_type) in [
        (f.exif.map(|e| [&[0u8; 4][..], e].concat()), b"Exif", None),
        (
            f.xmp.map(<[u8]>::to_vec),
            b"mime",
            Some("application/rdf+xml"),
        ),
    ] {
        if let Some(data) = payload {
            let id = b.item(kind, data);
            b.get(id).content_type = content_type;
            b.refs.push((*b"cdsc", id, vec![color]));
        }
    }

    let mut ftyp = b"avif\0\0\0\0avifmif1miaf".to_vec();
    if baseline {
        ftyp.extend_from_slice(b"MA1B");
    }
    if has_gain_map {
        ftyp.extend_from_slice(b"tmap");
    }
    let ftyp = bx(b"ftyp", &ftyp);
    // Offsets are fixed-width, so the meta size does not depend on them.
    let meta_len = meta(&b, color, altr.as_deref(), 0).len();
    let mdat_start = (ftyp.len() + meta_len + 8) as u32;
    let meta = meta(&b, color, altr.as_deref(), mdat_start);
    debug_assert_eq!(meta.len(), meta_len);
    let mdat: Vec<u8> = b
        .items
        .iter()
        .flat_map(|i| i.data.iter().copied())
        .collect();
    let mut out = Vec::with_capacity(ftyp.len() + meta.len() + mdat.len() + 8);
    out.extend(ftyp);
    out.extend(meta);
    out.extend(bx(b"mdat", &mdat));
    out
}

fn meta(b: &Builder, primary: u16, altr: Option<&[u16]>, mdat_start: u32) -> Vec<u8> {
    let mut hdlr = vec![0; 4];
    hdlr.extend_from_slice(b"pict");
    hdlr.extend_from_slice(&[0; 12]);
    hdlr.push(0);
    let mut body = full(b"hdlr", 0, 0, &hdlr);
    body.extend(full(b"pitm", 0, 0, &primary.to_be_bytes()));

    let mut iloc = vec![0x44, 0x00];
    iloc.extend_from_slice(&(b.items.len() as u16).to_be_bytes());
    let mut offset = mdat_start;
    for i in &b.items {
        iloc.extend_from_slice(&i.id.to_be_bytes());
        iloc.extend_from_slice(&[0, 0, 0, 1]); // data_reference_index, extent_count
        iloc.extend_from_slice(&offset.to_be_bytes());
        iloc.extend_from_slice(&(i.data.len() as u32).to_be_bytes());
        offset += i.data.len() as u32;
    }
    body.extend(full(b"iloc", 0, 0, &iloc));

    let mut iinf = (b.items.len() as u16).to_be_bytes().to_vec();
    for i in &b.items {
        let mut infe = i.id.to_be_bytes().to_vec();
        infe.extend_from_slice(&[0, 0]);
        infe.extend_from_slice(&i.kind);
        infe.push(0); // empty name
        if let Some(ct) = i.content_type {
            infe.extend_from_slice(ct.as_bytes());
            infe.push(0);
        }
        iinf.extend(full(b"infe", 2, u32::from(i.hidden), &infe));
    }
    body.extend(full(b"iinf", 0, 0, &iinf));

    if !b.refs.is_empty() {
        let mut iref = Vec::new();
        for (kind, from, to) in &b.refs {
            let mut r = from.to_be_bytes().to_vec();
            r.extend_from_slice(&(to.len() as u16).to_be_bytes());
            to.iter()
                .for_each(|t| r.extend_from_slice(&t.to_be_bytes()));
            iref.extend(bx(kind, &r));
        }
        body.extend(full(b"iref", 0, 0, &iref));
    }

    let ipco: Vec<u8> = b.props.concat();
    let wide = b.props.len() > 127;
    let with_props: Vec<&Item> = b.items.iter().filter(|i| !i.props.is_empty()).collect();
    let mut ipma = (with_props.len() as u32).to_be_bytes().to_vec();
    for i in with_props {
        ipma.extend_from_slice(&i.id.to_be_bytes());
        ipma.push(i.props.len() as u8);
        for &(p, essential) in &i.props {
            if wide {
                ipma.extend_from_slice(&((u16::from(essential) << 15) | p as u16).to_be_bytes());
            } else {
                ipma.push((u8::from(essential) << 7) | p as u8);
            }
        }
    }
    let mut iprp = bx(b"ipco", &ipco);
    iprp.extend(full(b"ipma", 0, u32::from(wide), &ipma));
    body.extend(bx(b"iprp", &iprp));

    if let Some(ids) = altr {
        let group_id = b.items.len() as u32 + 1;
        let mut a = group_id.to_be_bytes().to_vec();
        a.extend_from_slice(&(ids.len() as u32).to_be_bytes());
        ids.iter()
            .for_each(|id| a.extend_from_slice(&u32::from(*id).to_be_bytes()));
        body.extend(bx(b"grpl", &full(b"altr", 0, 0, &a)));
    }
    full(b"meta", 0, 0, &body)
}
