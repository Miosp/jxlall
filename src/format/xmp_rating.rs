//! Quality rating in XMP: the raw score under this tool's namespace, plus
//! optional `xmp:Rating` stars. A new `rdf:Description` carries the values;
//! properties it replaces are removed from the packet first.

use std::ops::Range;

use roxmltree::{Document, Node};

use super::xmp_extended::RDF_NS;
use crate::domain::Rating;

const XMP_NS: &str = "http://ns.adobe.com/xap/1.0/";
const RATING: &str = "Rating";
pub const IQA_NS: &str = "http://ns.photo-convert/iqa/1.0/";
const IQA_PREFIX: &str = "pciqa";
const SCORE_DECIMALS: usize = 4;
const PACKET_TRAILER: &[u8] = b"<?xpacket end=";
const WRITABLE: [&[u8]; 2] = [b"\"w\"", b"'w'"];

/// Fresh packet for a source without XMP.
pub fn new_packet(r: &Rating) -> Vec<u8> {
    let empty = format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"{RDF_NS}\"></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>"
    );
    apply(empty.as_bytes(), r).expect("template packet is well-formed")
}

/// `packet` with the rating merged in.
pub fn apply(packet: &[u8], r: &Rating) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(packet).map_err(|_| "XMP is not UTF-8")?;
    // Some writers NUL-terminate the payload; keep the tail as it was.
    let body = text.trim_end_matches('\0');
    let mut out = edit(body, r)?;
    out.push_str(&text[body.len()..]);
    Ok(out.into_bytes())
}

/// Like [`apply`], but the result keeps the packet's byte length: the
/// whitespace padding before a writable `<?xpacket end="w"?>` absorbs the
/// difference. For containers that record the packet size (JPEG
/// reconstruction data).
pub fn apply_same_length(packet: &[u8], r: &Rating) -> Result<Vec<u8>, String> {
    let mut out = apply(packet, r)?;
    let trailer = out
        .windows(PACKET_TRAILER.len())
        .rposition(|w| w == PACKET_TRAILER)
        .ok_or("XMP packet has no padding")?;
    let mode = &out[trailer + PACKET_TRAILER.len()..];
    if !WRITABLE.iter().any(|w| mode.starts_with(w)) {
        return Err("XMP packet is read-only".into());
    }
    let pad_start = out[..trailer]
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |p| p + 1);
    if out.len() > packet.len() {
        let grow = out.len() - packet.len();
        if trailer - pad_start < grow {
            return Err("not enough XMP padding for the rating".into());
        }
        out.drain(pad_start..pad_start + grow);
    } else {
        let shrink = packet.len() - out.len();
        out.splice(pad_start..pad_start, std::iter::repeat_n(b' ', shrink));
    }
    Ok(out)
}

fn edit(text: &str, r: &Rating) -> Result<String, String> {
    let doc = Document::parse(text).map_err(|e| format!("XMP: {e}"))?;
    let rdf = doc
        .descendants()
        .find(|n| n.has_tag_name((RDF_NS, "RDF")))
        .ok_or("XMP without rdf:RDF")?;
    let head = &text[..rdf.range().end];
    let insert_at = head
        .rfind("</")
        .filter(|_| !head.ends_with("/>"))
        .ok_or("self-closing rdf:RDF")?;

    let existing_stars = properties(&doc, XMP_NS, Some(RATING));
    let stars = r.stars.filter(|_| r.overwrite_stars || existing_stars.is_empty());
    let mut removals = properties(&doc, IQA_NS, None);
    if stars.is_some() {
        removals.extend(existing_stars);
    }
    removals.retain(|range| range.end <= insert_at);
    removals.sort_by_key(|range| range.start);

    let about = rdf
        .children()
        .filter(Node::is_element)
        .find_map(|d| d.attribute((RDF_NS, "about")))
        .unwrap_or("");
    let mut out = String::with_capacity(text.len() + 512);
    let mut pos = 0;
    for range in removals {
        // An attribute inside an element already removed.
        if range.start < pos {
            continue;
        }
        out.push_str(&text[pos..range.start]);
        pos = range.end;
    }
    out.push_str(&text[pos..insert_at]);
    out.push_str(&description(about, r, stars));
    out.push_str(&text[insert_at..]);
    Ok(out)
}

/// Source ranges of every property in `ns` (only `name`, if given), in
/// element or attribute form.
fn properties(doc: &Document, ns: &str, name: Option<&str>) -> Vec<Range<usize>> {
    let matches = |n_ns: Option<&str>, n_name: &str| n_ns == Some(ns) && name.is_none_or(|x| x == n_name);
    let mut out = Vec::new();
    for n in doc.descendants().filter(Node::is_element) {
        if matches(n.tag_name().namespace(), n.tag_name().name()) {
            out.push(n.range());
        }
        out.extend(
            n.attributes()
                .filter(|a| matches(a.namespace(), a.name()))
                .map(|a| a.range()),
        );
    }
    out
}

/// Self-contained `rdf:Description`: declares every prefix it uses, so the
/// packet's own prefix choices do not matter.
fn description(about: &str, r: &Rating, stars: Option<u8>) -> String {
    let mut d = format!(
        r#"<rdf:Description xmlns:rdf="{RDF_NS}" rdf:about="{about}" xmlns:{IQA_PREFIX}="{IQA_NS}" {IQA_PREFIX}:Score="{score:.prec$}" {IQA_PREFIX}:Model="{model}""#,
        about = escape(about),
        score = r.score,
        prec = SCORE_DECIMALS,
        model = escape(r.model),
    );
    if let Some(s) = stars {
        d.push_str(&format!(r#" xmlns:xmp="{XMP_NS}" xmp:Rating="{s}""#));
    }
    d.push_str("/>");
    d
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rating(stars: Option<u8>, overwrite_stars: bool) -> Rating {
        Rating {
            score: 0.61234,
            model: "test-model@1",
            stars,
            overwrite_stars,
        }
    }

    /// Values of every `ns`:`name` property, element or attribute form.
    fn values(packet: &[u8], ns: &str, name: &str) -> Vec<String> {
        let text = std::str::from_utf8(packet).unwrap().trim_end_matches('\0');
        let doc = Document::parse(text).expect("well-formed packet");
        let mut out = Vec::new();
        for n in doc.descendants().filter(Node::is_element) {
            if n.has_tag_name((ns, name)) {
                out.push(n.text().unwrap_or("").to_string());
            }
            out.extend(n.attribute((ns, name)).map(str::to_string));
        }
        out
    }

    fn packet(description: &str, padding: usize) -> String {
        format!(
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF_NS}">{description}</rdf:RDF></x:xmpmeta>{pad}<?xpacket end="w"?>"#,
            pad = " ".repeat(padding)
        )
    }

    #[test]
    fn new_packet_carries_score_model_and_stars() {
        let p = new_packet(&rating(Some(4), false));
        assert_eq!(values(&p, IQA_NS, "Score"), ["0.6123"]);
        assert_eq!(values(&p, IQA_NS, "Model"), ["test-model@1"]);
        assert_eq!(values(&p, XMP_NS, RATING), ["4"]);
    }

    #[test]
    fn stars_are_optional() {
        let p = new_packet(&rating(None, false));
        assert!(values(&p, XMP_NS, RATING).is_empty());
        assert_eq!(values(&p, IQA_NS, "Score").len(), 1);
    }

    #[test]
    fn existing_rating_is_kept_unless_overwrite() {
        let src = packet(
            &format!(r#"<rdf:Description rdf:about="" xmlns:xmp="{XMP_NS}"><xmp:Rating>2</xmp:Rating></rdf:Description>"#),
            0,
        );
        let kept = apply(src.as_bytes(), &rating(Some(5), false)).unwrap();
        assert_eq!(values(&kept, XMP_NS, RATING), ["2"]);
        assert_eq!(values(&kept, IQA_NS, "Score").len(), 1);
        let replaced = apply(src.as_bytes(), &rating(Some(5), true)).unwrap();
        assert_eq!(values(&replaced, XMP_NS, RATING), ["5"]);
    }

    #[test]
    fn previous_iqa_properties_are_replaced() {
        let src = packet(
            &format!(r#"<rdf:Description rdf:about="" xmlns:q="{IQA_NS}" q:Score="0.1" q:Model="old"/>"#),
            0,
        );
        let out = apply(src.as_bytes(), &rating(None, false)).unwrap();
        assert_eq!(values(&out, IQA_NS, "Score"), ["0.6123"]);
        assert_eq!(values(&out, IQA_NS, "Model"), ["test-model@1"]);
    }

    #[test]
    fn foreign_rdf_prefix_and_about_are_respected() {
        let src = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><R:RDF xmlns:R="{RDF_NS}"><R:Description R:about="uuid:a&amp;b"/></R:RDF></x:xmpmeta>"#
        );
        let out = apply(src.as_bytes(), &rating(Some(3), false)).unwrap();
        let text = String::from_utf8(out).unwrap();
        let doc = Document::parse(&text).unwrap();
        let abouts: Vec<_> = doc
            .descendants()
            .filter_map(|n| n.attribute((RDF_NS, "about")))
            .collect();
        assert_eq!(abouts, ["uuid:a&b", "uuid:a&b"]);
    }

    #[test]
    fn same_length_edit_consumes_padding() {
        let src = packet(r#"<rdf:Description rdf:about=""/>"#, 2048);
        let out = apply_same_length(src.as_bytes(), &rating(Some(4), false)).unwrap();
        assert_eq!(out.len(), src.len());
        assert_eq!(values(&out, XMP_NS, RATING), ["4"]);
    }

    #[test]
    fn same_length_edit_pads_when_packet_shrinks() {
        let long_model = "m".repeat(400);
        let src = packet(
            &format!(r#"<rdf:Description rdf:about="" xmlns:q="{IQA_NS}" q:Score="0.1" q:Model="{long_model}"/>"#),
            0,
        );
        let out = apply_same_length(src.as_bytes(), &rating(None, false)).unwrap();
        assert_eq!(out.len(), src.len());
        assert_eq!(values(&out, IQA_NS, "Model"), ["test-model@1"]);
    }

    #[test]
    fn same_length_edit_needs_room_and_a_writable_packet() {
        let tight = packet(r#"<rdf:Description rdf:about=""/>"#, 10);
        assert!(apply_same_length(tight.as_bytes(), &rating(None, false)).is_err());
        let read_only = packet(r#"<rdf:Description rdf:about=""/>"#, 2048).replace(r#"end="w""#, r#"end="r""#);
        assert!(apply_same_length(read_only.as_bytes(), &rating(None, false)).is_err());
        let bare = format!(r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF_NS}"></rdf:RDF></x:xmpmeta>"#);
        assert!(apply_same_length(bare.as_bytes(), &rating(None, false)).is_err());
    }

    #[test]
    fn trailing_nuls_are_preserved() {
        let src = format!("{}\0\0", packet(r#"<rdf:Description rdf:about=""/>"#, 1024));
        let out = apply_same_length(src.as_bytes(), &rating(None, false)).unwrap();
        assert_eq!(out.len(), src.len());
        assert!(out.ends_with(b"?>\0\0"));
    }
}
