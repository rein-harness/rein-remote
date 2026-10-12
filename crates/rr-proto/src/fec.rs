//! Video as datagrams, with forward error correction: a frame is cut into shards that each fit in one
//! datagram, and parity shards go with them, so a lost datagram costs nothing as long as no more are
//! lost than there's parity for (no retransmission, no waiting). A frame is late or whole, never
//! stalled behind another.
//!
//! The code is Reed-Solomon over GF(2^8), systematic (the data shards go as they are) with a Cauchy
//! matrix for the parity: any `k` of a block's `k + m` shards rebuild it. A frame bigger than one
//! block (255 shards) is cut into several blocks, each rebuilt on its own.
//!
//! Each datagram: magic (1), version (1), frame sequence (u32), block (u8), blocks in the frame
//! (u8), shard (u8), data shards `k` (u8), parity shards `m` (u8), the block's length in bytes (u24),
//! the datagram's own sequence number (u16, one more each datagram: the receiver counts the gaps for
//! the loss rate, as RTP does), then the shard. Little-endian, like the rest of the protocol.

use std::collections::BTreeMap;

pub const MAGIC: u8 = 0xD7;
pub const VERSION: u8 = 0;
pub const HEADER_LEN: usize = 16;
/// Shards in one block at most (data and parity): GF(2^8) has 256 elements, one is kept apart.
pub const MAX_SHARDS: usize = 255;

// ---- GF(2^8), the polynomial x^8 + x^4 + x^3 + x^2 + 1 (0x11d) ----

const fn tables() -> ([u8; 512], [u8; 256]) {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x: u16 = 1;
    let mut i = 0;
    while i < 255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= 0x11d;
        }
        i += 1;
    }
    let mut j = 255;
    while j < 512 {
        exp[j] = exp[j - 255];
        j += 1;
    }
    (exp, log)
}
const TABLES: ([u8; 512], [u8; 256]) = tables();
const EXP: [u8; 512] = TABLES.0;
const LOG: [u8; 256] = TABLES.1;

fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        EXP[LOG[a as usize] as usize + LOG[b as usize] as usize]
    }
}

fn inv(a: u8) -> u8 {
    debug_assert!(a != 0);
    EXP[255 - LOG[a as usize] as usize]
}

/// `dst ^= c * src`, byte by byte.
fn mul_add(dst: &mut [u8], c: u8, src: &[u8]) {
    if c == 0 {
        return;
    }
    let lc = LOG[c as usize] as usize;
    for (d, &s) in dst.iter_mut().zip(src) {
        if s != 0 {
            *d ^= EXP[lc + LOG[s as usize] as usize];
        }
    }
}

/// Row `j` of the Cauchy matrix, column `i`: 1 / (x_j + y_i) with x_j = k + j, y_i = i (all distinct,
/// so every square part of [identity; Cauchy] can be inverted).
fn cauchy(j: usize, i: usize, k: usize) -> u8 {
    inv(((k + j) as u8) ^ (i as u8))
}

/// The parity shards for `data` (each the same length).
pub fn encode_parity(data: &[&[u8]], m: usize) -> Vec<Vec<u8>> {
    let k = data.len();
    assert!(k > 0 && k + m <= MAX_SHARDS);
    let len = data[0].len();
    (0..m)
        .map(|j| {
            let mut p = vec![0u8; len];
            for (i, d) in data.iter().enumerate() {
                mul_add(&mut p, cauchy(j, i, k), d);
            }
            p
        })
        .collect()
}

/// The `k` data shards from any `k` of the `k + m` shards (`shards[i]`: shard `i`, data first, if it
/// came). None when fewer than `k` came. Only the missing data shards are computed: the k×k matrix
/// of the shards that came is inverted (small), and each missing shard is one row of it times them.
pub fn reconstruct(shards: &[Option<Vec<u8>>], k: usize) -> Option<Vec<Vec<u8>>> {
    if shards.iter().take(k).all(Option::is_some) {
        return Some(shards.iter().take(k).map(|s| s.clone().unwrap()).collect());
    }
    let have: Vec<usize> = shards.iter().enumerate().filter(|(_, s)| s.is_some()).map(|(i, _)| i).take(k).collect();
    if have.len() < k {
        return None;
    }
    let len = shards[have[0]].as_ref().unwrap().len();
    // The rows of the encoding matrix for the shards that came: identity for data, Cauchy for parity.
    let mut a: Vec<Vec<u8>> = have
        .iter()
        .map(|&r| (0..k).map(|c| if r < k { (r == c) as u8 } else { cauchy(r - k, c, k) }).collect())
        .collect();
    let inverse = invert(&mut a)?;
    let mut out = Vec::with_capacity(k);
    for i in 0..k {
        match &shards[i] {
            Some(s) => out.push(s.clone()),
            None => {
                let mut d = vec![0u8; len];
                for (j, &r) in have.iter().enumerate() {
                    mul_add(&mut d, inverse[i][j], shards[r].as_ref().unwrap());
                }
                out.push(d);
            }
        }
    }
    Some(out)
}

/// The inverse of a square matrix over GF(2^8) (Gauss-Jordan; `a` is used up), if it has one.
fn invert(a: &mut [Vec<u8>]) -> Option<Vec<Vec<u8>>> {
    let n = a.len();
    let mut inv_m: Vec<Vec<u8>> = (0..n).map(|r| (0..n).map(|c| (r == c) as u8).collect()).collect();
    for col in 0..n {
        let pivot = (col..n).find(|&r| a[r][col] != 0)?;
        a.swap(col, pivot);
        inv_m.swap(col, pivot);
        let p = inv(a[col][col]);
        for x in a[col].iter_mut() {
            *x = mul(*x, p);
        }
        for x in inv_m[col].iter_mut() {
            *x = mul(*x, p);
        }
        let (pa, pi) = (a[col].clone(), inv_m[col].clone());
        for r in 0..n {
            let f = a[r][col];
            if r != col && f != 0 {
                mul_add(&mut a[r], f, &pa);
                mul_add(&mut inv_m[r], f, &pi);
            }
        }
    }
    Some(inv_m)
}

// ---- frames as datagrams ----

/// One datagram's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shard {
    pub frame: u32,
    pub block: u8,
    pub blocks: u8,
    pub index: u8,
    pub k: u8,
    pub m: u8,
    /// The block's length in bytes (its last data shard is padded to the shard size).
    pub len: u32,
    /// This datagram's number in the sender's sequence (wraps).
    pub seq: u16,
}

impl Shard {
    pub fn read(d: &[u8]) -> Option<(Shard, &[u8])> {
        if d.len() < HEADER_LEN || d[0] != MAGIC || d[1] != VERSION {
            return None;
        }
        let s = Shard {
            frame: u32::from_le_bytes(d[2..6].try_into().ok()?),
            block: d[6],
            blocks: d[7],
            index: d[8],
            k: d[9],
            m: d[10],
            len: u32::from_le_bytes([d[11], d[12], d[13], 0]),
            seq: u16::from_le_bytes([d[14], d[15]]),
        };
        let ok = s.k > 0 && (s.k as usize + s.m as usize) <= MAX_SHARDS && s.index < s.k.saturating_add(s.m) && s.block < s.blocks;
        ok.then_some((s, &d[HEADER_LEN..]))
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[MAGIC, VERSION]);
        out.extend_from_slice(&self.frame.to_le_bytes());
        out.extend_from_slice(&[self.block, self.blocks, self.index, self.k, self.m]);
        out.extend_from_slice(&self.len.to_le_bytes()[..3]);
        out.extend_from_slice(&self.seq.to_le_bytes());
    }
}

/// `frame` (a whole protocol frame, e.g. a Video message) as datagrams of at most `max_datagram`
/// bytes, with parity for `parity` (0.0–1.0) of the data shards, at least one when it's above 0.
/// `next` is the sender's running datagram number.
pub fn split(frame_seq: u32, frame: &[u8], max_datagram: usize, parity: f32, next: &mut u16) -> Vec<Vec<u8>> {
    let shard_max = max_datagram.saturating_sub(HEADER_LEN).max(16);
    let parity = parity.clamp(0.0, 1.0);
    // Blocks of at most this many data shards, so data and parity fit in 255.
    let k_max = ((MAX_SHARDS as f32) / (1.0 + parity)).floor().max(1.0) as usize;
    let block_bytes = (k_max * shard_max).min((1 << 24) - 1);
    let blocks: Vec<&[u8]> = if frame.is_empty() {
        vec![&[][..]]
    } else {
        frame.chunks(block_bytes).collect()
    };
    let n = blocks.len().min(255);
    let mut out = Vec::new();
    for (b, block) in blocks.iter().take(n).enumerate() {
        let k = block.len().div_ceil(shard_max).max(1);
        let size = block.len().div_ceil(k).max(1);
        let m = if parity > 0.0 { ((k as f32 * parity).ceil() as usize).max(1) } else { 0 }.min(MAX_SHARDS - k);
        let data: Vec<Vec<u8>> = (0..k)
            .map(|i| {
                let mut s = block[(i * size).min(block.len())..((i + 1) * size).min(block.len())].to_vec();
                s.resize(size, 0);
                s
            })
            .collect();
        let refs: Vec<&[u8]> = data.iter().map(|d| d.as_slice()).collect();
        let parity_shards = if m > 0 { encode_parity(&refs, m) } else { Vec::new() };
        for (i, s) in data.iter().chain(parity_shards.iter()).enumerate() {
            let mut d = Vec::with_capacity(HEADER_LEN + s.len());
            Shard {
                frame: frame_seq,
                block: b as u8,
                blocks: n as u8,
                index: i as u8,
                k: k as u8,
                m: m as u8,
                len: block.len() as u32,
                seq: *next,
            }
            .write(&mut d);
            *next = next.wrapping_add(1);
            d.extend_from_slice(s);
            out.push(d);
        }
    }
    out
}

struct Partial {
    blocks: u8,
    shards: BTreeMap<u8, Block>,
}

/// One block of a frame on its way: its shards as they came, `k`, its length, and the block once rebuilt.
struct Block {
    shards: Vec<Option<Vec<u8>>>,
    k: u8,
    len: u32,
    data: Option<Vec<u8>>,
}

/// Frames put back together from datagrams in any order, lost ones rebuilt from parity. A frame
/// that can't be rebuilt by the time `window` newer frames have started is given up (counted as
/// lost; the client then asks for what it needs, e.g. a keyframe).
pub struct Reassembler {
    partial: BTreeMap<u32, Partial>,
    window: u32,
    /// The newest frame given back: anything older that comes after is late, and dropped.
    done_up_to: Option<u32>,
    pub lost: u64,
    pub recovered: u64,
    /// Datagrams expected and received, all told (summed over each `loss_permille` window).
    pub expected_total: u64,
    pub received_total: u64,
    /// Datagrams that came, and the sequence span they cover (first and highest), since `loss()`.
    received: u32,
    span: Option<(u16, u16)>,
    /// The highest sequence number the last `loss_permille` counted up to: the next count starts just
    /// after it, so a datagram lost between two counts (a client counts each frame, often one
    /// datagram) is still missed.
    counted: Option<u16>,
}

impl Reassembler {
    pub fn new(window: u32) -> Self {
        Self {
            partial: BTreeMap::new(),
            window: window.max(1),
            done_up_to: None,
            lost: 0,
            recovered: 0,
            expected_total: 0,
            received_total: 0,
            received: 0,
            span: None,
            counted: None,
        }
    }

    /// Of the datagrams sent since the last call (by their sequence numbers), the share that never
    /// came, in thousandths.
    pub fn loss_permille(&mut self) -> u16 {
        let Some((first, last)) = self.span.take() else { return 0 };
        let first = match self.counted {
            Some(c) if (last.wrapping_sub(c) as i16) > 0 => c.wrapping_add(1),
            // Only late ones since (already counted as missing): nothing new.
            Some(_) => {
                self.received = 0;
                return 0;
            }
            None => first,
        };
        self.counted = Some(last);
        let expected = last.wrapping_sub(first) as u32 + 1;
        let got = std::mem::take(&mut self.received).min(expected);
        self.expected_total += expected as u64;
        self.received_total += got as u64;
        ((expected - got) * 1000 / expected) as u16
    }

    /// One datagram in; the frame it completes, if it does.
    pub fn push(&mut self, datagram: &[u8]) -> Option<(u32, Vec<u8>)> {
        let (h, body) = Shard::read(datagram)?;
        self.received += 1;
        self.span = Some(match self.span {
            None => (h.seq, h.seq),
            // Datagrams can come out of order: the span grows either way.
            Some((first, last)) => (
                if (h.seq.wrapping_sub(first) as i16) < 0 { h.seq } else { first },
                if h.seq.wrapping_sub(last) as i16 > 0 { h.seq } else { last },
            ),
        });
        if self.done_up_to.is_some_and(|d| h.frame.wrapping_sub(d) as i32 <= 0) {
            return None; // late: a newer frame is already out
        }
        let p = self.partial.entry(h.frame).or_insert_with(|| Partial {
            blocks: h.blocks,
            shards: BTreeMap::new(),
        });
        let entry = p.shards.entry(h.block).or_insert_with(|| Block {
            shards: vec![None; h.k as usize + h.m as usize],
            k: h.k,
            len: h.len,
            data: None,
        });
        if entry.data.is_none() && (h.index as usize) < entry.shards.len() {
            entry.shards[h.index as usize] = Some(body.to_vec());
            let k = entry.k as usize;
            if entry.shards.iter().filter(|s| s.is_some()).count() >= k {
                let missing_data = entry.shards.iter().take(k).any(Option::is_none);
                if let Some(data) = reconstruct(&entry.shards, k) {
                    if missing_data {
                        self.recovered += 1;
                    }
                    let mut block: Vec<u8> = data.concat();
                    block.truncate(entry.len as usize);
                    entry.data = Some(block);
                    entry.shards.clear();
                }
            }
        }
        let p = &self.partial[&h.frame];
        let whole = p.shards.len() == p.blocks as usize && p.shards.values().all(|b| b.data.is_some());
        // Give up on frames too far behind the newest seen.
        let newest = *self.partial.keys().next_back().unwrap();
        let stale: Vec<u32> = self.partial.keys().copied().filter(|&f| newest.wrapping_sub(f) > self.window).collect();
        for f in stale {
            if f != h.frame {
                self.partial.remove(&f);
                self.lost += 1;
            }
        }
        if !whole {
            return None;
        }
        let p = self.partial.remove(&h.frame)?;
        // Older frames still waiting are now late: lost.
        let older: Vec<u32> = self.partial.keys().copied().filter(|&f| h.frame.wrapping_sub(f) as i32 > 0).collect();
        for f in older {
            self.partial.remove(&f);
            self.lost += 1;
        }
        self.done_up_to = Some(h.frame);
        Some((h.frame, p.shards.into_values().flat_map(|b| b.data.unwrap()).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(n: usize, seed: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    #[test]
    fn gf_inverse_and_tables() {
        for a in 1..=255u8 {
            assert_eq!(mul(a, inv(a)), 1);
        }
        assert_eq!(mul(0x53, 0xca), mul(0xca, 0x53));
    }

    #[test]
    fn any_k_of_k_plus_m_rebuild_the_data() {
        let data: Vec<Vec<u8>> = (0..6).map(|i| frame(100, i)).collect();
        let refs: Vec<&[u8]> = data.iter().map(|d| d.as_slice()).collect();
        let parity = encode_parity(&refs, 3);
        let all: Vec<Vec<u8>> = data.iter().chain(&parity).cloned().collect();
        // Every way to lose 3 of the 9.
        for a in 0..9 {
            for b in a + 1..9 {
                for c in b + 1..9 {
                    let got: Vec<Option<Vec<u8>>> = all.iter().enumerate().map(|(i, s)| (![a, b, c].contains(&i)).then(|| s.clone())).collect();
                    assert_eq!(reconstruct(&got, 6).unwrap(), data, "lost {a} {b} {c}");
                }
            }
        }
        // Four lost: too many.
        let got: Vec<Option<Vec<u8>>> = all.iter().enumerate().map(|(i, s)| (i >= 4).then(|| s.clone())).collect();
        assert!(reconstruct(&got, 6).is_none());
    }

    #[test]
    fn frames_survive_losses_up_to_their_parity_in_any_order() {
        for (len, parity) in [(1, 0.2), (1000, 0.2), (37_000, 0.1), (400_000, 0.25), (900_000, 0.05)] {
            let f = frame(len, 7);
            let mut d = split(42, &f, 1200, parity, &mut 0);
            assert!(d.iter().all(|x| x.len() <= 1200));
            // Lose one shard in each block where there's parity, then deliver backwards.
            let mut seen = std::collections::HashSet::new();
            d.retain(|x| {
                let (h, _) = Shard::read(x).unwrap();
                !(h.m > 0 && seen.insert(h.block))
            });
            d.reverse();
            let mut r = Reassembler::new(8);
            let out: Vec<_> = d.iter().filter_map(|x| r.push(x)).collect();
            assert_eq!(out, vec![(42, f)], "len {len}");
        }
    }

    #[test]
    fn a_frame_short_of_shards_is_given_up_when_a_newer_one_completes() {
        let mut r = Reassembler::new(4);
        let mut n = 0;
        let a = split(1, &frame(5000, 1), 1000, 0.0, &mut n);
        let b = split(2, &frame(3000, 2), 1000, 0.0, &mut n);
        for d in &a[1..] {
            assert!(r.push(d).is_none());
        }
        let got: Vec<_> = b.iter().filter_map(|d| r.push(d)).collect();
        assert_eq!(got.len(), 1);
        assert_eq!(r.lost, 1);
        // Frame 1's missing shard, late: dropped.
        assert!(r.push(&a[0]).is_none());
    }

    #[test]
    fn garbage_is_ignored() {
        let mut r = Reassembler::new(4);
        assert!(r.push(&[]).is_none());
        assert!(r.push(&[MAGIC, VERSION, 0, 0, 0, 0, 0, 1, 9, 0, 0, 0, 0, 0, 0, 0]).is_none()); // k = 0
        assert!(r.push(&[1, 2, 3]).is_none());
    }

    #[test]
    fn loss_is_counted_from_the_sequence_gaps() {
        let mut r = Reassembler::new(8);
        let mut n = 65_530; // wraps on the way
        let mut all = Vec::new();
        for f in 0..10 {
            all.extend(split(f, &frame(3000, f as u8), 1000, 0.5, &mut n));
        }
        // Every tenth lost, the rest backwards.
        for (i, d) in all.iter().enumerate().rev() {
            if i % 10 != 3 {
                r.push(d);
            }
        }
        assert_eq!(r.loss_permille(), 100);
        assert_eq!(r.loss_permille(), 0, "counted afresh after each call");
    }

    #[test]
    fn loss_between_two_counts_is_counted() {
        // Frames of one datagram each, counted after each frame (as a client's feedback does): a lost
        // one falls between two counts, and still counts.
        let mut r = Reassembler::new(8);
        let mut n = 65_500;
        let mut per_call = Vec::new();
        for f in 0..100u32 {
            let d = split(f, &frame(100, f as u8), 1200, 0.0, &mut n);
            if f % 10 != 5 {
                for x in &d {
                    r.push(x);
                }
                per_call.push(r.loss_permille());
            }
        }
        assert_eq!(per_call.iter().filter(|&&l| l == 500).count(), 10, "the count after each loss: 1 of 2");
        assert_eq!((r.expected_total, r.received_total), (100, 90), "all told");
    }

    /// The datagrams of a few frames, some lost, as the web client's test reads them
    /// (clients/web/test/fec-vectors.json): the same bytes must rebuild the same frames there.
    fn vectors() -> String {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let mut n = 65_000u16;
        let mut cases = Vec::new();
        for (f, (len, parity, drop_every)) in [(1usize, 0.2f32, 0usize), (2500, 0.3, 3), (12_000, 0.1, 17), (70_000, 0.05, 40)]
            .into_iter()
            .enumerate()
        {
            let data = frame(len, f as u8);
            let mut ds = split(f as u32 + 1, &data, 1200, parity, &mut n);
            if drop_every > 0 {
                let mut i = 0;
                ds.retain(|_| {
                    i += 1;
                    i % drop_every != 0
                });
            }
            ds.reverse();
            let list: Vec<String> = ds.iter().map(|d| format!("\"{}\"", hex(d))).collect();
            cases.push(format!("{{\"frame\":\"{}\",\"datagrams\":[{}]}}", hex(&data), list.join(",")));
        }
        format!("[{}]\n", cases.join(",\n"))
    }

    #[test]
    fn the_web_clients_vectors_are_current() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../clients/web/test/fec-vectors.json");
        let want = vectors();
        if std::env::var_os("RR_WRITE_VECTORS").is_some() {
            std::fs::write(path, &want).unwrap();
        }
        assert!(
            std::fs::read_to_string(path).is_ok_and(|have| have == want),
            "run with RR_WRITE_VECTORS=1 to update {path}"
        );
    }
}
