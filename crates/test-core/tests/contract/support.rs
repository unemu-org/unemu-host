// SPDX-License-Identifier: AGPL-3.0-or-later
//! Driving the test core as a front end does: through `&dyn Core` and
//! `Box<dyn Machine>`, with the contract's storage and content in memory.

use std::rc::Rc;
use unemu_core_api::test_support::{MemoryContent, MemoryStorage};
use unemu_core_api::{
    Content, ContentSource, Core, CreateRequest, Error, ErrorKind, Frame, Identified, Machine,
    Message, OfferedContent, RunLimit, Stop, Ticks,
};
use unemu_test_core::TestCore;

pub const CORE: &dyn Core = &TestCore;

/// A digest for a script. The core trusts the front end's digest and never
/// hashes, so any fixed value stands for the bytes.
pub fn digest_of(script: &str) -> [u8; 32] {
    let mut digest = [0u8; 32];
    for (index, byte) in script.bytes().enumerate() {
        digest[index % 32] = digest[index % 32].wrapping_mul(31).wrapping_add(byte);
    }
    digest
}

/// Content by digest, as a front end hands it over.
pub struct Source {
    pub sha256: [u8; 32],
    pub bytes: Vec<u8>,
    pub as_reader: bool,
}

impl ContentSource for Source {
    fn open(&self, sha256: &[u8; 32], size: u64) -> Result<Content, Error> {
        if *sha256 != self.sha256 || size != self.bytes.len() as u64 {
            return Err(Error::new(ErrorKind::Content, "no such content"));
        }
        Ok(if self.as_reader {
            Content::Reader(Box::new(MemoryContent::new(self.bytes.clone())))
        } else {
            Content::Bytes(self.bytes.clone())
        })
    }
}

pub fn identify(script: &str) -> Result<Option<Identified>, Error> {
    let content = MemoryContent::new(script.as_bytes().to_vec());
    CORE.identify(&OfferedContent::new(
        "test.utc",
        digest_of(script),
        script.len() as u64,
        &content,
    ))
}

/// A machine for `script` over `storage`, started as the generic launch
/// path starts one: identify, validate, create.
pub fn create_on(script: &str, storage: &MemoryStorage) -> Result<Box<dyn Machine>, Error> {
    let identified = identify(script)?.expect("the core recognises the script");
    let config = identified.config.as_document();
    let validated = CORE.validate(identified.platform, config)?;
    assert_eq!(validated.content.len(), 1);
    assert_eq!(validated.content[0].sha256, digest_of(script));
    let source = Source {
        sha256: digest_of(script),
        bytes: script.as_bytes().to_vec(),
        as_reader: false,
    };
    CORE.create(&CreateRequest::new(
        identified.platform,
        config,
        &source,
        Rc::new(storage.clone()),
    ))
}

pub fn create(script: &str) -> Box<dyn Machine> {
    create_on(script, &MemoryStorage::new()).expect("the script makes a machine")
}

pub fn run_to(machine: &mut dyn Machine, ticks: u64) -> Stop {
    machine
        .run(RunLimit::until(Ticks(ticks)))
        .expect("the run succeeds")
}

pub fn drain(machine: &mut dyn Machine) -> Vec<[i16; 2]> {
    let mut out = Vec::new();
    machine.drain_audio(&mut out).expect("sound drains");
    out
}

pub fn messages(machine: &mut dyn Machine) -> Vec<Message> {
    let mut out = Vec::new();
    machine.drain_messages(&mut out);
    out
}

/// The parts of a frame a test compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
    pub aspect: (u32, u32),
    pub serial: u64,
    pub visible: unemu_core_api::FrameArea,
}

impl Picture {
    pub fn at(&self, x: u32, y: u32) -> u32 {
        self.pixels[(y * self.width + x) as usize]
    }
}

pub fn picture(machine: &mut dyn Machine) -> Picture {
    let frame: Frame<'_> = machine
        .frame(unemu_core_api::DisplayId(0))
        .expect("display 0 has a picture");
    Picture {
        width: frame.width,
        height: frame.height,
        pixels: frame.pixels.to_vec(),
        aspect: frame.aspect,
        serial: frame.serial,
        visible: frame.visible,
    }
}
