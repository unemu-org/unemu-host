// SPDX-License-Identifier: AGPL-3.0-or-later
//! The counters a saved state holds, and their bytes.
//!
//! A state is the line `unemu-test-core state 1`, the machine's state
//! identity on the next line, then the counters in little-endian order:
//! the time, the time of the last start, the starts, the events done, the
//! status, then what the last finished frame shows and what the machine
//! holds now.

use crate::picture::Shown;
use unemu_core_api::{Error, ErrorKind, Status};

const FIRST_LINE: &[u8] = b"unemu-test-core state 1\n";

/// A pointer's pixel, if it points at the picture, and its held buttons.
pub(crate) type Pointer = (Option<(u32, u32)>, u8);

/// What the machine holds from its input ports now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    /// Per pad port, in port order: the held controls' bits.
    pub pads: Vec<u16>,
    /// Per keyboard port, in port order: the held keys, sorted.
    pub keys: Vec<Vec<u16>>,
    /// Per pointer port of either kind, in port order: its pixel and its
    /// held buttons.
    pub pointers: Vec<Pointer>,
}

/// Everything a saved state holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Counters {
    pub now: u64,
    pub started_at: u64,
    pub starts: u64,
    pub events_done: usize,
    pub status: Status,
    pub shown: Shown,
    pub held: Held,
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_pointers(out: &mut Vec<u8>, pointers: &[Pointer]) {
    put_u64(out, pointers.len() as u64);
    for &(place, buttons) in pointers {
        match place {
            Some((x, y)) => {
                out.push(1);
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            None => out.extend_from_slice(&[0; 9]),
        }
        out.push(buttons);
    }
}

fn put_masks(out: &mut Vec<u8>, masks: &[u16]) {
    put_u64(out, masks.len() as u64);
    for mask in masks {
        out.extend_from_slice(&mask.to_le_bytes());
    }
}

/// The bytes of a state.
pub(crate) fn encode(identity: &str, counters: &Counters) -> Vec<u8> {
    let mut out = FIRST_LINE.to_vec();
    out.extend_from_slice(identity.as_bytes());
    out.push(b'\n');
    put_u64(&mut out, counters.now);
    put_u64(&mut out, counters.started_at);
    put_u64(&mut out, counters.starts);
    put_u64(&mut out, counters.events_done as u64);
    out.push(match counters.status {
        Status::Halted => 1,
        Status::Off => 2,
        _ => 0,
    });
    put_u64(&mut out, counters.shown.frames);
    put_masks(&mut out, &counters.shown.pads);
    put_pointers(&mut out, &counters.shown.pointers);
    put_masks(&mut out, &counters.held.pads);
    put_u64(&mut out, counters.held.keys.len() as u64);
    for keys in &counters.held.keys {
        put_masks(&mut out, keys);
    }
    put_pointers(&mut out, &counters.held.pointers);
    out
}

fn refused(message: &str) -> Error {
    Error::new(ErrorKind::State, format!("the saved state {message}"))
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], Error> {
        if self.bytes.len() < count {
            return Err(refused("ends early"));
        }
        let (head, tail) = self.bytes.split_at(count);
        self.bytes = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn count(&mut self, expected: usize) -> Result<usize, Error> {
        if self.u64()? != expected as u64 {
            return Err(refused("has other input ports than this machine"));
        }
        Ok(expected)
    }

    fn masks(&mut self, expected: usize) -> Result<Vec<u16>, Error> {
        let count = self.count(expected)?;
        (0..count).map(|_| self.u16()).collect()
    }

    fn keys(&mut self) -> Result<Vec<u16>, Error> {
        let count = self.u64()?;
        if count > u64::from(u16::MAX) + 1 {
            return Err(refused("holds more keys than a keyboard has"));
        }
        let keys: Vec<u16> = (0..count).map(|_| self.u16()).collect::<Result<_, _>>()?;
        if keys.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(refused("holds keys out of order"));
        }
        Ok(keys)
    }

    fn pointers(
        &mut self,
        expected: usize,
        width: u32,
        height: u32,
    ) -> Result<Vec<Pointer>, Error> {
        let count = self.count(expected)?;
        (0..count)
            .map(|_| {
                let present = self.u8()?;
                let (x, y) = (self.u32()?, self.u32()?);
                let buttons = self.u8()?;
                let place = match present {
                    0 if x == 0 && y == 0 => None,
                    1 if x < width && y < height => Some((x, y)),
                    _ => return Err(refused("holds a pointer off the picture")),
                };
                Ok((place, buttons))
            })
            .collect()
    }
}

/// The shape of the machine a state must fit.
pub(crate) struct Shape {
    pub pads: usize,
    /// The pad masks a frame shows, at most two.
    pub shown_pads: usize,
    pub keyboards: usize,
    pub pointers: usize,
    pub events: usize,
    pub width: u32,
    pub height: u32,
}

/// Read a state saved by a machine with the same identity and shape.
pub(crate) fn decode(identity: &str, shape: &Shape, bytes: &[u8]) -> Result<Counters, Error> {
    let rest = bytes
        .strip_prefix(FIRST_LINE)
        .ok_or_else(|| refused("is not one the test core wrote"))?;
    let rest = rest
        .strip_prefix(identity.as_bytes())
        .and_then(|rest| rest.strip_prefix(b"\n"))
        .ok_or_else(|| refused("was saved by another release of the core or for another script"))?;
    let mut reader = Reader { bytes: rest };
    let now = reader.u64()?;
    let started_at = reader.u64()?;
    let starts = reader.u64()?;
    let events_done = reader.u64()?;
    let status = match reader.u8()? {
        0 => Status::Ready,
        1 => Status::Halted,
        2 => Status::Off,
        _ => return Err(refused("holds an unknown status")),
    };
    if started_at > now || events_done > shape.events as u64 {
        return Err(refused("holds counters this machine cannot reach"));
    }
    let shown = Shown {
        frames: reader.u64()?,
        pads: reader.masks(shape.shown_pads)?,
        pointers: reader.pointers(shape.pointers, shape.width, shape.height)?,
    };
    let pads = reader.masks(shape.pads)?;
    let keyboards = reader.count(shape.keyboards)?;
    let keys = (0..keyboards)
        .map(|_| reader.keys())
        .collect::<Result<_, _>>()?;
    let pointers = reader.pointers(shape.pointers, shape.width, shape.height)?;
    if !reader.bytes.is_empty() {
        return Err(refused("has bytes after its counters"));
    }
    Ok(Counters {
        now,
        started_at,
        starts,
        events_done: events_done as usize,
        status,
        shown,
        held: Held {
            pads,
            keys,
            pointers,
        },
    })
}
