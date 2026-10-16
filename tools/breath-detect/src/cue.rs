//! Écrit un WAV avec marqueurs pour RX : un `cue ` point + une region `ltxt`
//! par souffle (labellisés "breath N"). On copie fmt+data de l'entrée tels
//! quels (audio intact) et on ajoute les chunks cue + LIST/adtl.

use serde::Deserialize;
use std::io;
use std::path::Path;

#[derive(Deserialize)]
struct RegionIn { s: f64, e: f64 }
#[derive(Deserialize)]
struct RegionsFile { regions: Vec<RegionIn> }

fn u32le(v: u32) -> [u8; 4] { v.to_le_bytes() }
fn u16le(v: u16) -> [u8; 2] { v.to_le_bytes() }

/// Trouve (offset_body, taille) d'un chunk par son id dans un RIFF/WAVE.
fn find_chunk<'a>(bytes: &'a [u8], id: &[u8; 4]) -> Option<(&'a [u8],)> {
    let mut pos = 12; // après "RIFF"+size+"WAVE"
    while pos + 8 <= bytes.len() {
        let cid = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]) as usize;
        let body_start = pos + 8;
        if body_start + size > bytes.len() { break; }
        if cid == id { return Some((&bytes[body_start..body_start + size],)); }
        pos = body_start + size + (size & 1); // pad pair
    }
    None
}

fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&u32le(body.len() as u32));
    out.extend_from_slice(body);
    if body.len() & 1 == 1 { out.push(0); } // pad pair
}

pub fn write_cue_wav(in_path: &Path, out_path: &Path, regions_json: &Path) -> io::Result<usize> {
    let bytes = std::fs::read(in_path)?;
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "pas un WAV RIFF"));
    }
    let fmt = find_chunk(&bytes, b"fmt ").ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "fmt absent"))?.0;
    let data = find_chunk(&bytes, b"data").ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "data absent"))?.0;
    let sr = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]); // nSamplesPerSec

    let rf: RegionsFile = serde_json::from_str(&std::fs::read_to_string(regions_json)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("JSON régions: {e}")))?;

    // cue chunk
    let mut cue = Vec::new();
    cue.extend_from_slice(&u32le(rf.regions.len() as u32));
    for (i, r) in rf.regions.iter().enumerate() {
        let off = (r.s * sr as f64).round() as u32;
        cue.extend_from_slice(&u32le(i as u32 + 1));   // dwIdentifier
        cue.extend_from_slice(&u32le(off));            // dwPosition
        cue.extend_from_slice(b"data");                // fccChunk
        cue.extend_from_slice(&u32le(0));              // dwChunkStart
        cue.extend_from_slice(&u32le(0));              // dwBlockStart
        cue.extend_from_slice(&u32le(off));            // dwSampleOffset
    }

    // LIST/adtl : labl + ltxt (region) par souffle
    let mut adtl = Vec::new();
    adtl.extend_from_slice(b"adtl");
    for (i, r) in rf.regions.iter().enumerate() {
        let id = i as u32 + 1;
        let label = format!("breath {}", id);
        // labl
        let mut labl = Vec::new();
        labl.extend_from_slice(&u32le(id));
        labl.extend_from_slice(label.as_bytes());
        labl.push(0);
        push_chunk(&mut adtl, b"labl", &labl);
        // ltxt (region) — layout canonique
        let len = ((r.e - r.s) * sr as f64).round().max(0.0) as u32;
        let mut ltxt = Vec::new();
        ltxt.extend_from_slice(&u32le(id));     // dwName (cue id)
        ltxt.extend_from_slice(&u32le(len));    // dwSampleLength
        ltxt.extend_from_slice(b"rgn ");        // dwPurpose
        ltxt.extend_from_slice(&u16le(0));      // wCountry
        ltxt.extend_from_slice(&u16le(0));      // wLanguage
        ltxt.extend_from_slice(&u16le(0));      // wDialect
        ltxt.extend_from_slice(&u16le(0));      // wCodePage
        ltxt.extend_from_slice(label.as_bytes());
        push_chunk(&mut adtl, b"ltxt", &ltxt);
    }

    // assemblage : WAVE + fmt + data + cue + LIST
    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    push_chunk(&mut body, b"fmt ", fmt);
    push_chunk(&mut body, b"data", data);
    push_chunk(&mut body, b"cue ", &cue);
    push_chunk(&mut body, b"LIST", &adtl);

    let mut riff = Vec::new();
    riff.extend_from_slice(b"RIFF");
    riff.extend_from_slice(&u32le(body.len() as u32));
    riff.extend_from_slice(&body);

    std::fs::write(out_path, &riff)?;
    Ok(rf.regions.len())
}
