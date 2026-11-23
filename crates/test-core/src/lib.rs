// SPDX-License-Identifier: AGPL-3.0-or-later
//! A scripted core for the front end's tests: it implements the whole core
//! contract and emulates nothing (core-contract.md §13.3, "Testing before a
//! NES exists").
//!
//! The front end's crates test against [`TestCore`] instead of a real
//! platform, so none of them names an emulator crate (core-contract.md
//! §7.3). It depends on the contract alone, forbids unsafe code and touches
//! nothing of the host: what it reads comes through the contract's
//! [`ContentSource`](unemu_core_api::ContentSource) and what outlives a run
//! goes to records of the contract's [`Storage`](unemu_core_api::Storage).
//! It is in no program's list of cores.
//!
//! # The content: a script
//!
//! A script is a text file, extension `utc`, whose first line is
//! `unemu-test-core 1`. Every later line is blank, a comment starting with
//! `#`, or a keyword and its words, separated by spaces. A `TEXT` or a
//! `LABEL` is the rest of the line. A `RATE` is `N` or `N/D`, each part from
//! 1 to 2³¹ − 1. Every line may appear once unless it says otherwise. A bad
//! line is refused with a message that names it (`script line 7: …`):
//! `identify` answers [`ErrorKind::Unsupported`] with it, because the core
//! recognises the file and cannot run it, and `create` answers
//! [`ErrorKind::Content`], because the content a config names is not a
//! script it can run.
//!
//! | Line | Default | Means |
//! |---|---|---|
//! | `title TEXT` | none | [`Identified::title`] |
//! | `notice TEXT`, any number | none | a warning in [`Identified::notes`] |
//! | `unsupported TEXT` | none | `identify` and `create` refuse the script with [`ErrorKind::Unsupported`] and this reason |
//! | `picture WIDTH HEIGHT` | `64 48` | the picture's size, 1 to 4096 each way |
//! | `aspect W H` | the size, reduced | [`Frame::aspect`](unemu_core_api::Frame::aspect) |
//! | `visible X Y WIDTH HEIGHT` | the whole picture | [`Frame::visible`](unemu_core_api::Frame::visible), within the picture |
//! | `clock RATE` | `60000` | ticks per second |
//! | `step TICKS` | `1` | the ticks of one step, 1 to 2⁵⁰ / 1000; every boundary is a whole step |
//! | `frame TICKS` | 1000 steps | a frame finishes at every multiple of this, a whole number of steps, at most 2⁵⁰ ticks |
//! | `sound RATE TONE` | no sound | frames per second (8 000 to 192 000) and the square wave's frequency in Hz, a `RATE` too |
//! | `keyboard PORT [LABEL]`, `pointer PORT BUTTONS [LABEL]`, `absolute PORT BUTTONS [LABEL]`, `pad PORT CONTROLS [LABEL]`, any number | none | an input port: a keyboard, a relative pointer, an absolute pointer on display 0, a pad with the comma-separated controls among `up,down,left,right,a,b,x,y,l,r,start,select`; at most 8 buttons |
//! | `slot SLOT KIND removable\|fixed FORMATS [LABEL]`, any number | none | a slot of kind `floppy`, `fixed-disk`, `optical`, `cartridge` or `tape` taking the comma-separated formats |
//! | `medium SLOT FORMAT SHA256 SIZE [NAME]`, any number | none | a read-only catalogue entry of a slot declared above; the machine never opens it |
//! | `insert SLOT ENTRY`, once per slot | empty | the entry inserted at creation |
//! | `resets soft\|hard\|soft,hard` | none | the kinds of reset |
//! | `saved-state`, `frame-stops`, `media-changes` | off | the machine's [`Features`] |
//! | `battery` | none | battery-backed memory in the record [`BATTERY_RECORD`] |
//! | `echo-input` | off | every input event reports a note when it reaches the machine |
//! | `at TICKS EVENT`, any number | none | `halt`, `power-off`, `guest-reset`, `fail TEXT`, `fail-content TEXT`, `note TEXT` or `warning TEXT` at the first boundary at or after `TICKS`; one past the end of virtual time never comes |
//!
//! ```text
//! unemu-test-core 1
//! title Two players
//! picture 256 240
//! aspect 128 105
//! visible 0 8 256 224
//! clock 21477272/4
//! step 4
//! frame 89344
//! sound 984375/22 440
//! pad 0 up,down,left,right,a,b,start,select Controller 1
//! pad 1 up,down,left,right,a,b,start,select Controller 2
//! absolute 2 1 Zapper
//! slot 0 cartridge fixed utc Cartridge
//! resets soft,hard
//! saved-state
//! frame-stops
//! battery
//! at 53693180 note ten seconds in
//! ```
//!
//! # The machine
//!
//! Exact and trivially predictable:
//!
//! - **Time.** A run jumps from boundary to boundary in whole steps.
//!   `steps()` is `now() / step`. A deadline is met at the first boundary at
//!   or after it. At one boundary the events come first, in script order,
//!   then a frame stop, then the deadline, then the step budget. `halt`
//!   leaves the machine [`Status::Halted`](unemu_core_api::Status), `power-off`
//!   [`Status::Off`](unemu_core_api::Status), `fail` fails the run with
//!   [`ErrorKind::Guest`] and `fail-content` with [`ErrorKind::Content`],
//!   each leaving the machine failed; `guest-reset` is a soft reset done
//!   inside the run and reported as a note. A deadline or a step budget past
//!   the end of virtual time is refused with [`ErrorKind::Usage`] and the
//!   machine stays as it was.
//! - **Input** queued by `input` takes effect at the start of the next run.
//! - **The picture** is drawn when a frame finishes, so it is always a
//!   finished one: see [`picture`].
//! - **The sound** is a square wave whose frame `i` is the sound at
//!   `i × clock / rate` ticks: see [`sound`]. Sound is on from creation.
//! - **Resets.** Soft and hard restart the frame count; with `battery`, hard
//!   also counts a start. Held controls stay held.
//! - **Battery memory** counts the machine's starts: its creation and every
//!   hard reset. It is written with `replace` and `commit` at `finish`, and
//!   at the end of a run once it changed and ten virtual seconds passed
//!   since the last write.
//! - **A saved state** is the counters: the time, the last start, the
//!   starts, the events done, the status, what the last frame shows and what
//!   is held. It loads only into a machine with an equal
//!   [`Machine::state_identity`],
//!   which names this release and the script's digest.
//! - **Media** are catalogue entries only. A medium added with
//!   [`WriteTarget::Kept`](unemu_core_api::WriteTarget) has its record opened
//!   when it is inserted, at the next run, and made durable at `finish`.
//!
//! # The config
//!
//! `identify` writes a config of type [`CONFIG_TYPE`] that names the
//! script by digest and size; `validate` lists that one piece of content and
//! `create` asks its content source for it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
mod machine;
pub mod picture;
mod script;
pub mod sound;
mod state;

pub use crate::config::CONFIG_TYPE;
pub use crate::machine::BATTERY_RECORD;

use crate::config::ScriptRef;
use unemu_core_api::{
    ConfigDocument, Content, ContentKind, ContentReader, ContentRef, ContractVersion, Core,
    CoreDescription, CreateRequest, Error, ErrorKind, Features, Identified, Machine, Message,
    MessageLevel, OfferedContent, PlatformDescription, ResetKind, Validated,
};

/// The core's id.
pub const CORE_ID: &str = "unemu-test-core";

/// The platform's id.
pub const PLATFORM: &str = "unemu-test";

/// The id of the one kind of content: a script.
pub const SCRIPT_KIND: &str = "script";

static RESETS: [ResetKind; 2] = [ResetKind::Soft, ResetKind::Hard];

static KINDS: [ContentKind; 1] = [ContentKind::new(
    SCRIPT_KIND,
    "unemu test core scripts",
    &["utc"],
)];

static PLATFORMS: [PlatformDescription; 1] = [PlatformDescription::new(PLATFORM, "Test machine")
    .with_config_types(&[CONFIG_TYPE])
    .with_features(
        Features::NONE
            .with_resets(&RESETS)
            .with_saved_state(true)
            .with_media_changes(true)
            .with_frame_stops(true),
    )
    .with_content(&KINDS)];

static DESCRIPTION: CoreDescription = CoreDescription::new(
    ContractVersion::new(0, 3),
    CORE_ID,
    "unemu test core",
    env!("CARGO_PKG_VERSION"),
)
.with_platforms(&PLATFORMS);

/// The scripted core.
#[derive(Clone, Copy, Debug, Default)]
pub struct TestCore;

/// All of `reader`'s bytes; `Unsupported` past the largest script.
fn read_script(reader: &dyn ContentReader) -> Result<Vec<u8>, Error> {
    let size = reader.size();
    if size > script::MAX_SCRIPT {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "the test core reads scripts of at most 1 MiB",
        ));
    }
    let mut bytes = vec![0; size as usize];
    reader.read_at(0, &mut bytes)?;
    Ok(bytes)
}

fn check_platform(platform: &str) -> Result<(), Error> {
    if platform == PLATFORM {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Unsupported,
            format!("the test core has no platform {platform}"),
        ))
    }
}

impl Core for TestCore {
    fn description(&self) -> &CoreDescription {
        &DESCRIPTION
    }

    fn create(&self, request: &CreateRequest<'_>) -> Result<Box<dyn Machine>, Error> {
        check_platform(request.platform)?;
        let wanted = config::read(request.config)?;
        if let Some((key, _)) = request.options.first() {
            return Err(Error::new(
                ErrorKind::Config,
                format!("the test core takes no option {key}"),
            ));
        }
        let bytes = match request.content.open(&wanted.sha256, wanted.size)? {
            Content::Bytes(bytes) => bytes,
            Content::Reader(reader) => read_script(reader.as_ref())?,
            _ => {
                return Err(Error::new(
                    ErrorKind::Content,
                    "the script came in a form the test core does not read",
                ))
            }
        };
        if bytes.len() as u64 != wanted.size {
            return Err(Error::new(
                ErrorKind::Content,
                "the script is not the size its config names",
            ));
        }
        let script = script::parse(&bytes)?;
        if let Some(reason) = script.unsupported.clone() {
            return Err(Error::new(ErrorKind::Unsupported, reason));
        }
        let machine = machine::TestMachine::new(script, &wanted.sha256, request.storage.clone())?;
        Ok(Box::new(machine))
    }

    /// `None` for bytes whose first line is not `unemu-test-core 1`.
    fn identify(&self, content: &OfferedContent<'_>) -> Result<Option<Identified>, Error> {
        let size = content.bytes.size();
        let mut head = vec![0; size.min(script::HEADER.len() as u64 + 2) as usize];
        content.bytes.read_at(0, &mut head)?;
        if !script::has_header(&head) {
            return Ok(None);
        }
        // A malformed script is a file the core recognises and cannot run.
        // `Content` from identify means the bytes changed after hashing.
        let script = script::parse(&read_script(content.bytes)?)
            .map_err(|error| Error::new(ErrorKind::Unsupported, error.message))?;
        if let Some(reason) = script.unsupported {
            return Err(Error::new(ErrorKind::Unsupported, reason));
        }
        let config = config::write(ScriptRef {
            sha256: content.sha256,
            size,
        });
        Ok(Some(
            Identified::new(PLATFORM, SCRIPT_KIND, config)
                .with_title(script.title)
                .with_notes(
                    script
                        .notices
                        .into_iter()
                        .map(|text| Message::new(MessageLevel::Warning, text))
                        .collect(),
                ),
        ))
    }

    fn validate(&self, platform: &str, config: ConfigDocument<'_>) -> Result<Validated, Error> {
        check_platform(platform)?;
        let script = config::read(config)?;
        Ok(Validated::new(vec![ContentRef::new(
            script.sha256,
            script.size,
            "script",
        )]))
    }
}
