// SPDX-License-Identifier: AGPL-3.0-or-later
//! The script a test core's content holds, read line by line; the grammar
//! is in the crate's documentation.

use unemu_core_api::{
    AudioDescription, Error, ErrorKind, FrameArea, MediumInfo, PadControl, PortId, Rate, ResetKind,
    SlotId, SlotKind, WriteKind,
};

/// The first line of every script.
pub(crate) const HEADER: &str = "unemu-test-core 1";

/// The largest script the core reads, in bytes.
pub(crate) const MAX_SCRIPT: u64 = 1 << 20;

/// The largest side of a picture, in pixels.
const MAX_SIDE: u32 = 4096;

/// The most ticks a frame takes. Keeps the frame arithmetic far from the
/// end of 64-bit virtual time.
pub(crate) const MAX_FRAME: u64 = 1 << 50;

/// The most ticks a step takes: a frame of the default thousand steps still
/// fits within [`MAX_FRAME`].
pub(crate) const MAX_STEP: u64 = MAX_FRAME / 1000;

/// The largest numerator or denominator of a rate. Keeps every product of the
/// time and sound arithmetic inside 128 bits.
const MAX_RATE_PART: u64 = (1 << 31) - 1;

/// Whether `bytes` starts with the header line, ended by a line break or by
/// the end of the bytes.
pub(crate) fn has_header(bytes: &[u8]) -> bool {
    match bytes.strip_prefix(HEADER.as_bytes()) {
        Some(rest) => rest.is_empty() || rest.starts_with(b"\n") || rest.starts_with(b"\r\n"),
        None => false,
    }
}

/// A whole script, checked.
#[derive(Clone, Debug)]
pub(crate) struct Script {
    pub title: Option<String>,
    pub notices: Vec<String>,
    pub unsupported: Option<String>,
    pub width: u32,
    pub height: u32,
    pub aspect: (u32, u32),
    pub visible: FrameArea,
    pub clock: Rate,
    pub step: u64,
    pub frame: u64,
    pub sound: Option<Sound>,
    pub ports: Vec<Port>,
    pub slots: Vec<Slot>,
    pub resets: &'static [ResetKind],
    pub saved_state: bool,
    pub frame_stops: bool,
    pub media_changes: bool,
    pub battery: bool,
    pub echo_input: bool,
    /// Sorted by time; events at one time keep the script's order.
    pub events: Vec<Event>,
}

/// The sound: its rate and the frequency of its square wave.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sound {
    pub rate: Rate,
    pub tone: Rate,
}

/// One input port.
#[derive(Clone, Debug)]
pub(crate) struct Port {
    pub id: PortId,
    pub kind: PortKind,
    pub label: String,
}

/// What an input port is.
#[derive(Clone, Debug)]
pub(crate) enum PortKind {
    Keyboard,
    Pointer { buttons: u8 },
    Absolute { buttons: u8 },
    Pad { controls: Vec<PadControl> },
}

/// One slot with its catalogue.
#[derive(Clone, Debug)]
pub(crate) struct Slot {
    pub id: SlotId,
    pub kind: SlotKind,
    pub removable: bool,
    pub formats: Vec<String>,
    pub label: String,
    pub catalogue: Vec<MediumInfo>,
    pub inserted: Option<usize>,
}

/// Something that happens at a virtual time.
#[derive(Clone, Debug)]
pub(crate) struct Event {
    pub at: u64,
    pub what: EventKind,
}

/// What happens.
#[derive(Clone, Debug)]
pub(crate) enum EventKind {
    Halt,
    PowerOff,
    GuestReset,
    Fail(String),
    /// The run fails as if the content's bytes were no longer the verified
    /// ones.
    FailContent(String),
    Note(String),
    Warning(String),
}

/// The order of controls in a field's colour, and their script names.
pub(crate) const CONTROLS: [(PadControl, &str); 12] = [
    (PadControl::Up, "up"),
    (PadControl::Down, "down"),
    (PadControl::Left, "left"),
    (PadControl::Right, "right"),
    (PadControl::A, "a"),
    (PadControl::B, "b"),
    (PadControl::X, "x"),
    (PadControl::Y, "y"),
    (PadControl::L, "l"),
    (PadControl::R, "r"),
    (PadControl::Start, "start"),
    (PadControl::Select, "select"),
];

static RESETS_NONE: [ResetKind; 0] = [];
static RESETS_SOFT: [ResetKind; 1] = [ResetKind::Soft];
static RESETS_HARD: [ResetKind; 1] = [ResetKind::Hard];
static RESETS_BOTH: [ResetKind; 2] = [ResetKind::Soft, ResetKind::Hard];

/// One line being read: its number and the words not read yet.
struct Line<'a> {
    number: usize,
    rest: &'a str,
}

impl<'a> Line<'a> {
    fn fail(&self, message: impl std::fmt::Display) -> Error {
        Error::new(
            ErrorKind::Content,
            format!("script line {}: {message}", self.number),
        )
    }

    fn word(&mut self, what: &str) -> Result<&'a str, Error> {
        let trimmed = self.rest.trim_start();
        if trimmed.is_empty() {
            return Err(self.fail(format_args!("{what} is missing")));
        }
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        self.rest = &trimmed[end..];
        Ok(&trimmed[..end])
    }

    /// The rest of the line, trimmed; `None` when nothing is left.
    fn text(&mut self) -> Option<String> {
        let text = self.rest.trim();
        self.rest = "";
        (!text.is_empty()).then(|| text.to_owned())
    }

    fn required_text(&mut self, what: &str) -> Result<String, Error> {
        self.text()
            .ok_or_else(|| self.fail(format_args!("{what} is missing")))
    }

    fn end(&self) -> Result<(), Error> {
        let rest = self.rest.trim();
        if rest.is_empty() {
            Ok(())
        } else {
            Err(self.fail(format_args!("unexpected `{rest}` at the end of the line")))
        }
    }

    fn int<T: std::str::FromStr>(&mut self, what: &str) -> Result<T, Error> {
        let word = self.word(what)?;
        word.parse()
            .map_err(|_| self.fail(format_args!("{what} `{word}` is not a number in range")))
    }

    /// `N` or `N/D`, each part from 1 to 2^31 - 1.
    fn rate(&mut self, what: &str) -> Result<Rate, Error> {
        let word = self.word(what)?;
        let (numerator, denominator) = match word.split_once('/') {
            Some((n, d)) => (n, d),
            None => (word, "1"),
        };
        let parts = (numerator.parse::<u64>(), denominator.parse::<u64>());
        match parts {
            (Ok(n), Ok(d))
                if (1..=MAX_RATE_PART).contains(&n) && (1..=MAX_RATE_PART).contains(&d) =>
            {
                Ok(Rate::new(n, d))
            }
            _ => Err(self.fail(format_args!(
                "{what} `{word}` is not N or N/D with each part from 1 to {MAX_RATE_PART}"
            ))),
        }
    }
}

fn once<T>(line: &Line<'_>, slot: &mut Option<T>, value: T, keyword: &str) -> Result<(), Error> {
    if slot.is_some() {
        return Err(line.fail(format_args!("`{keyword}` is given twice")));
    }
    *slot = Some(value);
    Ok(())
}

fn flag(line: &Line<'_>, slot: &mut bool, keyword: &str) -> Result<(), Error> {
    line.end()?;
    if *slot {
        return Err(line.fail(format_args!("`{keyword}` is given twice")));
    }
    *slot = true;
    Ok(())
}

fn hex_digest(line: &Line<'_>, word: &str) -> Result<[u8; 32], Error> {
    let bytes = word.as_bytes();
    let invalid = || line.fail(format_args!("`{word}` is not 64 lower-case hex digits"));
    if bytes.len() != 64 {
        return Err(invalid());
    }
    let mut digest = [0u8; 32];
    for (index, pair) in bytes.chunks(2).enumerate() {
        let high = hex_value(pair[0]).ok_or_else(invalid)?;
        let low = hex_value(pair[1]).ok_or_else(invalid)?;
        digest[index] = high << 4 | low;
    }
    Ok(digest)
}

pub(crate) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Read a script. Every refusal is a `Content` error that names the line.
pub(crate) fn parse(bytes: &[u8]) -> Result<Script, Error> {
    let first = Line {
        number: 1,
        rest: "",
    };
    if !has_header(bytes) {
        return Err(first.fail(format_args!("the first line is not `{HEADER}`")));
    }
    let mut picture = None;
    let mut aspect = None;
    let mut visible = None;
    let mut clock = None;
    let mut step = None;
    let mut frame = None;
    let mut sound = None;
    let mut resets = None;
    let mut title = None;
    let mut unsupported = None;
    let mut notices = Vec::new();
    let mut ports: Vec<Port> = Vec::new();
    let mut slots: Vec<Slot> = Vec::new();
    let mut events = Vec::new();
    let mut inserts = Vec::new();
    let (mut saved_state, mut frame_stops, mut media_changes, mut battery, mut echo_input) =
        (false, false, false, false, false);

    for (index, raw) in bytes.split(|&b| b == b'\n').enumerate().skip(1) {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let mut line = Line {
            number: index + 1,
            rest: "",
        };
        line.rest =
            std::str::from_utf8(raw).map_err(|_| line.fail("the line is not UTF-8 text"))?;
        let trimmed = line.rest.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let keyword = line.word("a keyword")?;
        match keyword {
            "title" => {
                let text = line.required_text("the title")?;
                once(&line, &mut title, text, keyword)?;
            }
            "notice" => notices.push(line.required_text("the notice")?),
            "unsupported" => {
                let reason = line.required_text("the reason")?;
                once(&line, &mut unsupported, reason, keyword)?;
            }
            "picture" => {
                let width: u32 = line.int("the width")?;
                let height: u32 = line.int("the height")?;
                line.end()?;
                if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
                    return Err(line.fail(format_args!(
                        "a picture is 1 to {MAX_SIDE} pixels wide and high"
                    )));
                }
                once(&line, &mut picture, (width, height), keyword)?;
            }
            "aspect" => {
                let width: u32 = line.int("the aspect's width")?;
                let height: u32 = line.int("the aspect's height")?;
                line.end()?;
                if width == 0 || height == 0 {
                    return Err(line.fail("an aspect has no zero side"));
                }
                once(&line, &mut aspect, (width, height), keyword)?;
            }
            "visible" => {
                let area = FrameArea {
                    x: line.int("the column")?,
                    y: line.int("the row")?,
                    width: line.int("the width")?,
                    height: line.int("the height")?,
                };
                line.end()?;
                if area.width == 0 || area.height == 0 {
                    return Err(line.fail("the visible part is not empty"));
                }
                once(&line, &mut visible, (area, line.number), keyword)?;
            }
            "clock" => {
                let rate = line.rate("the clock")?;
                line.end()?;
                once(&line, &mut clock, rate, keyword)?;
            }
            "step" => {
                let ticks: u64 = line.int("the ticks of a step")?;
                line.end()?;
                if !(1..=MAX_STEP).contains(&ticks) {
                    return Err(line.fail(format_args!("a step takes 1 to {MAX_STEP} ticks")));
                }
                once(&line, &mut step, ticks, keyword)?;
            }
            "frame" => {
                let ticks: u64 = line.int("the ticks of a frame")?;
                line.end()?;
                if !(1..=MAX_FRAME).contains(&ticks) {
                    return Err(line.fail(format_args!("a frame takes 1 to {MAX_FRAME} ticks")));
                }
                once(&line, &mut frame, (ticks, line.number), keyword)?;
            }
            "sound" => {
                let rate = line.rate("the sound's rate")?;
                let tone = line.rate("the tone's frequency")?;
                line.end()?;
                if !AudioDescription::new(rate).rate_in_bounds() {
                    return Err(line.fail(format_args!(
                        "the sound's rate is from {} to {} frames per second",
                        AudioDescription::MIN_RATE,
                        AudioDescription::MAX_RATE
                    )));
                }
                once(&line, &mut sound, Sound { rate, tone }, keyword)?;
            }
            "keyboard" | "pointer" | "absolute" | "pad" => {
                let id = PortId(line.int("the port")?);
                let kind = match keyword {
                    "keyboard" => PortKind::Keyboard,
                    "pointer" | "absolute" => {
                        let buttons: u8 = line.int("the number of buttons")?;
                        if buttons > 8 {
                            return Err(line.fail("a pointer has at most 8 buttons"));
                        }
                        if keyword == "pointer" {
                            PortKind::Pointer { buttons }
                        } else {
                            PortKind::Absolute { buttons }
                        }
                    }
                    _ => {
                        let names = line.word("the controls")?;
                        let mut controls = Vec::new();
                        for name in names.split(',') {
                            let control = CONTROLS
                                .iter()
                                .find(|(_, known)| *known == name)
                                .map(|(control, _)| *control)
                                .ok_or_else(|| {
                                    line.fail(format_args!("`{name}` is not a pad control"))
                                })?;
                            if controls.contains(&control) {
                                return Err(line.fail(format_args!("`{name}` is listed twice")));
                            }
                            controls.push(control);
                        }
                        PortKind::Pad { controls }
                    }
                };
                if ports.iter().any(|port| port.id == id) {
                    return Err(line.fail(format_args!("port {} is given twice", id.0)));
                }
                let label = line.text().unwrap_or_else(|| match &kind {
                    PortKind::Keyboard => "Keyboard".to_owned(),
                    PortKind::Pointer { .. } => "Mouse".to_owned(),
                    PortKind::Absolute { .. } => "Light gun".to_owned(),
                    PortKind::Pad { .. } => format!("Pad {}", id.0),
                });
                ports.push(Port { id, kind, label });
            }
            "slot" => {
                let id = SlotId(line.int("the slot")?);
                let kind = match line.word("the slot's kind")? {
                    "floppy" => SlotKind::Floppy,
                    "fixed-disk" => SlotKind::FixedDisk,
                    "optical" => SlotKind::Optical,
                    "cartridge" => SlotKind::Cartridge,
                    "tape" => SlotKind::Tape,
                    other => {
                        return Err(line.fail(format_args!(
                            "`{other}` is not floppy, fixed-disk, optical, cartridge or tape"
                        )))
                    }
                };
                let removable = match line.word("`removable` or `fixed`")? {
                    "removable" => true,
                    "fixed" => false,
                    other => {
                        return Err(line.fail(format_args!("`{other}` is not removable or fixed")))
                    }
                };
                let formats: Vec<String> = line
                    .word("the formats")?
                    .split(',')
                    .map(str::to_owned)
                    .collect();
                if formats.iter().any(String::is_empty) {
                    return Err(line.fail("a format is not empty"));
                }
                if slots.iter().any(|slot| slot.id == id) {
                    return Err(line.fail(format_args!("slot {} is given twice", id.0)));
                }
                let label = line.text().unwrap_or_else(|| format!("Slot {}", id.0));
                slots.push(Slot {
                    id,
                    kind,
                    removable,
                    formats,
                    label,
                    catalogue: Vec::new(),
                    inserted: None,
                });
            }
            "medium" => {
                let id = SlotId(line.int("the slot")?);
                let format = line.word("the format")?.to_owned();
                let digest = line.word("the SHA-256")?;
                let sha256 = hex_digest(&line, digest)?;
                let size: u64 = line.int("the size")?;
                let name = line.text().unwrap_or_else(|| "medium".to_owned());
                let slot = slots
                    .iter_mut()
                    .find(|slot| slot.id == id)
                    .ok_or_else(|| line.fail(format_args!("no slot {} before this line", id.0)))?;
                if !slot.formats.contains(&format) {
                    return Err(line.fail(format_args!(
                        "slot {} does not take the format `{format}`",
                        id.0
                    )));
                }
                slot.catalogue.push(MediumInfo::new(
                    name,
                    sha256,
                    size,
                    format,
                    WriteKind::ReadOnly,
                ));
            }
            "insert" => {
                let id = SlotId(line.int("the slot")?);
                let entry: usize = line.int("the catalogue entry")?;
                line.end()?;
                inserts.push((line.number, id, entry));
            }
            "resets" => {
                let words = line.word("the kinds of reset")?;
                line.end()?;
                let (mut soft, mut hard) = (false, false);
                for word in words.split(',') {
                    let seen = match word {
                        "soft" => &mut soft,
                        "hard" => &mut hard,
                        other => {
                            return Err(line.fail(format_args!("`{other}` is not soft or hard")))
                        }
                    };
                    if *seen {
                        return Err(line.fail(format_args!("`{word}` is listed twice")));
                    }
                    *seen = true;
                }
                let kinds: &'static [ResetKind] = match (soft, hard) {
                    (true, true) => &RESETS_BOTH,
                    (true, false) => &RESETS_SOFT,
                    (false, true) => &RESETS_HARD,
                    (false, false) => &RESETS_NONE,
                };
                once(&line, &mut resets, kinds, keyword)?;
            }
            "saved-state" => flag(&line, &mut saved_state, keyword)?,
            "frame-stops" => flag(&line, &mut frame_stops, keyword)?,
            "media-changes" => flag(&line, &mut media_changes, keyword)?,
            "battery" => flag(&line, &mut battery, keyword)?,
            "echo-input" => flag(&line, &mut echo_input, keyword)?,
            "at" => {
                let at: u64 = line.int("the time")?;
                let what = match line.word("the event")? {
                    "halt" => EventKind::Halt,
                    "power-off" => EventKind::PowerOff,
                    "guest-reset" => EventKind::GuestReset,
                    "fail" => EventKind::Fail(line.required_text("the failure")?),
                    "fail-content" => EventKind::FailContent(line.required_text("the failure")?),
                    "note" => EventKind::Note(line.required_text("the note")?),
                    "warning" => EventKind::Warning(line.required_text("the warning")?),
                    other => {
                        return Err(line.fail(format_args!(
                            "`{other}` is not halt, power-off, guest-reset, fail, fail-content, note or warning"
                        )))
                    }
                };
                line.end()?;
                events.push(Event { at, what });
            }
            other => return Err(line.fail(format_args!("unknown keyword `{other}`"))),
        }
    }

    let (width, height) = picture.unwrap_or((64, 48));
    let whole = FrameArea {
        x: 0,
        y: 0,
        width,
        height,
    };
    let visible = match visible {
        Some((area, number)) => {
            if !area.is_within(width, height) {
                return Err(
                    Line { number, rest: "" }.fail("the visible part lies outside the picture")
                );
            }
            area
        }
        None => whole,
    };
    let step = step.unwrap_or(1);
    let frame = match frame {
        Some((ticks, number)) => {
            if ticks % step != 0 {
                return Err(Line { number, rest: "" }.fail("a frame takes a whole number of steps"));
            }
            ticks
        }
        // MAX_STEP keeps this within MAX_FRAME.
        None => step * 1000,
    };
    for (number, id, entry) in inserts {
        let line = Line { number, rest: "" };
        let slot = slots
            .iter_mut()
            .find(|slot| slot.id == id)
            .ok_or_else(|| line.fail(format_args!("there is no slot {}", id.0)))?;
        if entry >= slot.catalogue.len() {
            return Err(line.fail(format_args!(
                "slot {} has {} catalogue entries",
                id.0,
                slot.catalogue.len()
            )));
        }
        if slot.inserted.is_some() {
            return Err(line.fail(format_args!("slot {} already holds a medium", id.0)));
        }
        slot.inserted = Some(entry);
    }
    ports.sort_by_key(|port| port.id);
    slots.sort_by_key(|slot| slot.id);
    events.sort_by_key(|event| event.at);
    Ok(Script {
        title,
        notices,
        unsupported,
        width,
        height,
        aspect: aspect.unwrap_or_else(|| reduced(width, height)),
        visible,
        clock: clock.unwrap_or(Rate::new(60_000, 1)),
        step,
        frame,
        sound,
        ports,
        slots,
        resets: resets.unwrap_or(&RESETS_NONE),
        saved_state,
        frame_stops,
        media_changes,
        battery,
        echo_input,
        events,
    })
}

fn reduced(width: u32, height: u32) -> (u32, u32) {
    let (mut a, mut b) = (width, height);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    (width / a, height / a)
}
