//! Extended XMP (XMP spec part 3, 1.1.3.1): a packet over the 64 KiB APP1
//! limit continues in `http://ns.adobe.com/xmp/extension/` segments, keyed
//! by the GUID the main packet names in `xmpNote:HasExtendedXMP`. JXL has a
//! single `xml ` box, so the extension is merged into the main packet.

use roxmltree::{Document, Node};

pub const EXTENSION_SIG: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
/// GUID (32 hex digits), full length (u32 BE), chunk offset (u32 BE).
const GUID_LEN: usize = 32;
const HEADER_LEN: usize = EXTENSION_SIG.len() + GUID_LEN + 8;
const XMP_NOTE_NS: &str = "http://ns.adobe.com/xmp/note/";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const HAS_EXTENDED: &str = "HasExtendedXMP";

pub struct Chunk<'a> {
    guid: &'a [u8],
    full_len: u32,
    offset: u32,
    data: &'a [u8],
}

/// Parse an APP1 body that starts with [`EXTENSION_SIG`].
pub fn parse_chunk(body: &[u8]) -> Option<Chunk<'_>> {
    let rest = body.strip_prefix(EXTENSION_SIG)?;
    if body.len() < HEADER_LEN {
        return None;
    }
    let (guid, rest) = rest.split_at(GUID_LEN);
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    Some(Chunk {
        guid,
        full_len: be32(&rest[..4]),
        offset: be32(&rest[4..8]),
        data: &rest[8..],
    })
}

/// Main packet with the extension's top-level `rdf:Description`s appended
/// to its `rdf:RDF` and the `HasExtendedXMP` pointer removed. A packet
/// without the pointer is returned unchanged (unreferenced chunks are
/// ignored, per spec).
pub fn merge(main: &[u8], chunks: &[Chunk]) -> Result<Vec<u8>, String> {
    // Some writers NUL-terminate the payload; trimming the end keeps offsets.
    let main_str = std::str::from_utf8(main)
        .map_err(|_| "main XMP is not UTF-8")?
        .trim_end_matches('\0');
    let doc = Document::parse(main_str).map_err(|e| format!("main XMP: {e}"))?;
    let Some((guid, pointer)) = extension_pointer(&doc) else {
        return Ok(main.to_vec());
    };
    let ext = assemble(chunks, guid.as_bytes()).ok_or("incomplete extended XMP chunks")?;
    let ext_str = std::str::from_utf8(&ext)
        .map_err(|_| "extended XMP is not UTF-8")?
        .trim_end_matches('\0');
    let ext_doc = Document::parse(ext_str).map_err(|e| format!("extended XMP: {e}"))?;

    let main_rdf = rdf(&doc)?;
    let rdf_range = main_rdf.range();
    let insert_at = main_str[..rdf_range.end]
        .rfind("</")
        .filter(|_| !main_str[..rdf_range.end].ends_with("/>"))
        .ok_or("self-closing rdf:RDF")?;
    if pointer.end > insert_at {
        return Err("HasExtendedXMP outside rdf:RDF".into());
    }
    let mut descriptions = String::new();
    for d in rdf(&ext_doc)?.children().filter(Node::is_element) {
        descriptions.push_str(&with_inherited_namespaces(ext_str, d));
    }
    Ok([
        &main_str[..pointer.start],
        &main_str[pointer.end..insert_at],
        &descriptions,
        &main_str[insert_at..],
    ]
    .concat()
    .into_bytes())
}

fn rdf<'a, 'i>(doc: &'a Document<'i>) -> Result<Node<'a, 'i>, &'static str> {
    doc.descendants()
        .find(|n| n.has_tag_name((RDF_NS, "RDF")))
        .ok_or("XMP without rdf:RDF")
}

/// GUID named by `xmpNote:HasExtendedXMP` (attribute or element form) and
/// the byte range to delete.
fn extension_pointer(doc: &Document<'_>) -> Option<(String, std::ops::Range<usize>)> {
    doc.descendants().find_map(|n| {
        if n.has_tag_name((XMP_NOTE_NS, HAS_EXTENDED)) {
            return Some((n.text()?.trim().to_string(), n.range()));
        }
        n.attributes()
            .find(|a| a.namespace() == Some(XMP_NOTE_NS) && a.name() == HAS_EXTENDED)
            .map(|a| (a.value().trim().to_string(), a.range()))
    })
}

/// Concatenate the GUID's chunks; `None` unless they tile `0..full_len`
/// exactly (also bounds the allocation by bytes actually present).
fn assemble(chunks: &[Chunk], guid: &[u8]) -> Option<Vec<u8>> {
    let mut parts: Vec<&Chunk> = chunks.iter().filter(|c| c.guid == guid).collect();
    let full_len = parts.first()?.full_len;
    parts.sort_by_key(|c| c.offset);
    let mut out = Vec::new();
    for c in parts {
        if c.full_len != full_len || c.offset as usize != out.len() {
            return None;
        }
        out.extend_from_slice(c.data);
    }
    (out.len() == full_len as usize).then_some(out)
}

/// Source text of `node` with the namespace declarations it inherits from
/// its ancestors made explicit, so it stays valid when moved elsewhere.
fn with_inherited_namespaces(src: &str, node: Node) -> String {
    let text = &src[node.range()];
    let name_end = text
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(text.len());
    let declared = declared_prefixes(text);
    let mut decls = String::new();
    for ns in node.namespaces() {
        if ns.name() == Some("xml") || declared.contains(&ns.name()) {
            continue;
        }
        let uri = ns
            .uri()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('"', "&quot;");
        match ns.name() {
            Some(p) => decls.push_str(&format!(" xmlns:{p}=\"{uri}\"")),
            None => decls.push_str(&format!(" xmlns=\"{uri}\"")),
        }
    }
    [&text[..name_end], &decls, &text[name_end..]].concat()
}

/// Prefixes (`None` = default namespace) declared in the element's own
/// start tag.
fn declared_prefixes(text: &str) -> Vec<Option<&str>> {
    let mut out = Vec::new();
    let mut quote = None;
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => break,
            None if b.is_ascii_whitespace() && text[i + 1..].starts_with("xmlns") => {
                let attr = &text[i + 1..];
                let name = attr[..attr.find('=').unwrap_or(attr.len())].trim_end();
                match name.strip_prefix("xmlns") {
                    Some("") => out.push(None),
                    Some(p) if p.starts_with(':') => out.push(Some(&p[1..])),
                    _ => {}
                }
            }
            None => {}
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const GUID: &str = "0123456789ABCDEF0123456789ABCDEF";

    pub fn main_packet() -> String {
        format!(
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF_NS}"><rdf:Description rdf:about="" xmlns:xmpNote="{XMP_NOTE_NS}" xmlns:dc="http://purl.org/dc/elements/1.1/" dc:format="image/jpeg" xmpNote:HasExtendedXMP="{GUID}"/></rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#
        )
    }

    /// Namespaces declared on ancestors only, plus one redundant redeclaration.
    pub fn extended_packet() -> String {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:GDepth="http://ns.google.com/photos/1.0/depthmap/"><rdf:RDF xmlns:rdf="{RDF_NS}"><rdf:Description xmlns:rdf="{RDF_NS}" rdf:about="" GDepth:Data="AAAA"/></rdf:RDF></x:xmpmeta>"#
        )
    }

    /// Extension APP1 bodies for `ext`, split in two and emitted out of order.
    pub fn chunk_bodies(ext: &[u8]) -> Vec<Vec<u8>> {
        let mid = ext.len() / 2;
        [(mid, &ext[mid..]), (0, &ext[..mid])]
            .iter()
            .map(|(offset, data)| {
                let mut b = EXTENSION_SIG.to_vec();
                b.extend_from_slice(GUID.as_bytes());
                b.extend_from_slice(&(ext.len() as u32).to_be_bytes());
                b.extend_from_slice(&(*offset as u32).to_be_bytes());
                b.extend_from_slice(data);
                b
            })
            .collect()
    }

    #[test]
    fn merges_extension_and_drops_pointer() {
        let bodies = chunk_bodies(extended_packet().as_bytes());
        let chunks: Vec<Chunk> = bodies.iter().map(|b| parse_chunk(b).unwrap()).collect();
        let merged = String::from_utf8(merge(main_packet().as_bytes(), &chunks).unwrap()).unwrap();
        let doc = Document::parse(&merged).expect("merged packet is well-formed");
        let depth = doc
            .descendants()
            .flat_map(|n| n.attributes())
            .find(|a| a.name() == "Data")
            .expect("extension property present");
        assert_eq!(
            depth.namespace(),
            Some("http://ns.google.com/photos/1.0/depthmap/")
        );
        assert_eq!(depth.value(), "AAAA");
        assert!(extension_pointer(&doc).is_none());
        assert!(merged.contains(r#"dc:format="image/jpeg""#));
    }

    #[test]
    fn nul_terminated_packets_still_merge() {
        let ext = format!("{}\0", extended_packet());
        let bodies = chunk_bodies(ext.as_bytes());
        let chunks: Vec<Chunk> = bodies.iter().map(|b| parse_chunk(b).unwrap()).collect();
        let main = format!("{}\0\0", main_packet());
        let merged = merge(main.as_bytes(), &chunks).unwrap();
        assert!(String::from_utf8(merged).unwrap().contains("GDepth:Data"));
    }

    #[test]
    fn self_closing_rdf_is_an_error() {
        let main = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" xmlns:xmpNote="{XMP_NOTE_NS}" xmpNote:HasExtendedXMP="{GUID}"><a></a><rdf:RDF xmlns:rdf="{RDF_NS}"/></x:xmpmeta>"#
        );
        let bodies = chunk_bodies(extended_packet().as_bytes());
        let chunks: Vec<Chunk> = bodies.iter().map(|b| parse_chunk(b).unwrap()).collect();
        assert!(merge(main.as_bytes(), &chunks).is_err());
    }

    #[test]
    fn missing_chunk_is_an_error() {
        let bodies = chunk_bodies(extended_packet().as_bytes());
        let chunks = vec![parse_chunk(&bodies[0]).unwrap()];
        assert!(merge(main_packet().as_bytes(), &chunks).is_err());
    }

    #[test]
    fn unreferenced_extension_is_ignored() {
        let main = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF_NS}"/></x:xmpmeta>"#
        );
        let bodies = chunk_bodies(extended_packet().as_bytes());
        let chunks: Vec<Chunk> = bodies.iter().map(|b| parse_chunk(b).unwrap()).collect();
        assert_eq!(merge(main.as_bytes(), &chunks).unwrap(), main.as_bytes());
    }

    #[test]
    fn declared_prefixes_skip_quoted_text() {
        let tag = r#"<a xmlns:p="u" q='xmlns:z="v"' xmlns = "d">"#;
        assert_eq!(declared_prefixes(tag), [Some("p"), None]);
    }
}
