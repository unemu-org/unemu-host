// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three answers of `identify`, `validate`, and the refusals of a bad
//! script and a bad config.

use crate::support::{create_on, digest_of, identify, Source, CORE};
use std::rc::Rc;
use unemu_core_api::test_support::{MemoryContent, MemoryStorage};
use unemu_core_api::{ConfigDocument, CreateRequest, ErrorKind, MessageLevel, OfferedContent};
use unemu_test_core::{CONFIG_TYPE, PLATFORM, SCRIPT_KIND};

#[test]
fn bytes_without_the_first_line_are_not_a_script() {
    for bytes in [
        "",
        "unemu-test-core",
        "unemu-test-core 2\n",
        "unemu-test-core 1x\n",
        " unemu-test-core 1\n",
        "\u{1a}NES and other bytes",
    ] {
        assert_eq!(
            identify(bytes).expect("a foreign file is no failure"),
            None,
            "{bytes:?}"
        );
    }
}

#[test]
fn an_unsupported_line_is_recognised_and_refused_with_its_reason() {
    let error = identify("unemu-test-core 1\nunsupported mapper 99 is not modelled\n")
        .expect_err("recognised content it cannot run");
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(error.message, "mapper 99 is not modelled");
}

#[test]
fn a_script_gets_a_config_that_names_it_by_digest() {
    let script = "unemu-test-core 1\r\ntitle Pattern\nnotice check the header\n";
    let identified = identify(script)
        .expect("a script is identified")
        .expect("and recognised");
    assert_eq!(identified.platform, PLATFORM);
    assert_eq!(identified.kind, SCRIPT_KIND);
    assert_eq!(identified.title.as_deref(), Some("Pattern"));
    assert_eq!(identified.notes.len(), 1);
    assert_eq!(identified.notes[0].level, MessageLevel::Warning);
    assert_eq!(identified.notes[0].text, "check the header");
    assert_eq!(identified.config.media_type, CONFIG_TYPE);
    let text = String::from_utf8(identified.config.bytes.clone()).expect("the config is text");
    assert!(
        !text.contains("test.utc"),
        "the config names no file: {text}"
    );
    let validated = CORE
        .validate(PLATFORM, identified.config.as_document())
        .expect("validate accepts what identify wrote");
    assert_eq!(validated.content.len(), 1);
    assert_eq!(validated.content[0].sha256, digest_of(script));
    assert_eq!(validated.content[0].size, script.len() as u64);
    // The same bytes give the same answer.
    assert_eq!(identify(script).expect("again"), Some(identified));
}

#[test]
fn a_bare_first_line_is_a_whole_script() {
    assert!(identify("unemu-test-core 1").expect("read").is_some());
    create_on("unemu-test-core 1", &MemoryStorage::new()).expect("a default machine");
}

#[test]
fn identify_reports_a_failed_read() {
    let content = MemoryContent::new(b"unemu-test-core 1\n".to_vec());
    content.fail_next(unemu_core_api::Error::new(
        ErrorKind::Storage,
        "read failed",
    ));
    let error = CORE
        .identify(&OfferedContent::new("x.utc", [0; 32], 18, &content))
        .expect_err("the read failed");
    assert_eq!(error.kind, ErrorKind::Storage);
}

#[test]
fn a_bad_script_is_refused_with_its_line() {
    let cases = [
        (
            "unemu-test-core 1\n\n# fine\nfrobnicate\n",
            "script line 4: unknown keyword `frobnicate`",
        ),
        ("unemu-test-core 1\npicture 0 10\n", "script line 2:"),
        (
            "unemu-test-core 1\npicture 10 10\npicture 10 10\n",
            "script line 3: `picture` is given twice",
        ),
        (
            "unemu-test-core 1\nsound 4000 440\n",
            "script line 2: the sound's rate",
        ),
        (
            "unemu-test-core 1\npad 0 up,jump\n",
            "script line 2: `jump` is not a pad control",
        ),
        (
            "unemu-test-core 1\nstep 3\nframe 10\n",
            "script line 3: a frame takes a whole number of steps",
        ),
        (
            "unemu-test-core 1\npicture 8 8\nvisible 4 4 8 8\n",
            "script line 3: the visible part lies outside",
        ),
        ("unemu-test-core 1\nmedium 0 utc 00 1\n", "script line 2:"),
        (
            "unemu-test-core 1\nat soon halt\n",
            "script line 2: the time `soon`",
        ),
        (
            "unemu-test-core 1\nkeyboard 1\npad 1 a\n",
            "script line 3: port 1 is given twice",
        ),
        (
            "unemu-test-core 1\nbattery extra\n",
            "script line 2: unexpected `extra`",
        ),
        // Hostile numbers: each is refused, none overflows.
        (
            "unemu-test-core 1\nstep 18446744073709551615\n",
            "script line 2: a step takes 1 to 1125899906842 ticks",
        ),
        (
            "unemu-test-core 1\nstep 1125899906843\n",
            "script line 2: a step takes 1 to",
        ),
        ("unemu-test-core 1\nstep 0\n", "script line 2: a step takes 1 to"),
        (
            "unemu-test-core 1\nframe 18446744073709551615\n",
            "script line 2: a frame takes 1 to 1125899906842624 ticks",
        ),
        ("unemu-test-core 1\nframe 0\n", "script line 2: a frame takes 1 to"),
        (
            "unemu-test-core 1\nstep 18446744073709551616\n",
            "script line 2: the ticks of a step `18446744073709551616` is not a number in range",
        ),
        (
            "unemu-test-core 1\nat 18446744073709551616 halt\n",
            "script line 2: the time",
        ),
        ("unemu-test-core 1\npicture 4097 1\n", "script line 2: a picture is 1 to 4096"),
        ("unemu-test-core 1\naspect 0 1\n", "script line 2: an aspect has no zero side"),
        (
            "unemu-test-core 1\npicture 8 8\nvisible 4294967295 0 2 2\n",
            "script line 3: the visible part lies outside",
        ),
        (
            "unemu-test-core 1\nvisible 0 0 4294967295 4294967295\n",
            "script line 2: the visible part lies outside",
        ),
        ("unemu-test-core 1\nclock 0\n", "script line 2: the clock `0`"),
        (
            "unemu-test-core 1\nclock 4294967296/3\n",
            "script line 2: the clock `4294967296/3`",
        ),
        (
            "unemu-test-core 1\nsound 48000/0 440\n",
            "script line 2: the sound's rate",
        ),
        ("unemu-test-core 1\npointer 0 9\n", "script line 2: a pointer has at most 8"),
        ("unemu-test-core 1\npad 256 a\n", "script line 2: the port `256`"),
        (
            "unemu-test-core 1\nslot 0 floppy removable utc\ninsert 0 18446744073709551615\n",
            "script line 3: slot 0 has 0 catalogue entries",
        ),
        (
            "unemu-test-core 1\nslot 0 floppy removable utc\nmedium 0 utc 0101010101010101010101010101010101010101010101010101010101010101 18446744073709551616\n",
            "script line 3: the size",
        ),
    ];
    for (script, expected) in cases {
        // Recognised and unable to run: `Content` from identify would mean
        // the bytes changed after hashing.
        let error = identify(script).expect_err("identify refuses a bad script");
        assert_eq!(error.kind, ErrorKind::Unsupported, "{script:?}");
        assert!(
            error.message.starts_with(expected),
            "{script:?}: {}",
            error.message
        );
        // `create` refuses the same script with the same message, as content
        // the config named that is no script it runs.
        let config = format!(
            "unemu-test-core-config 1\nscript {} {}\n",
            hex(&digest_of(script)),
            script.len()
        );
        let source = Source {
            sha256: digest_of(script),
            bytes: script.as_bytes().to_vec(),
            as_reader: true,
        };
        let refused = CORE
            .create(&CreateRequest::new(
                PLATFORM,
                ConfigDocument::new(CONFIG_TYPE, config.as_bytes()),
                &source,
                Rc::new(MemoryStorage::new()),
            ))
            .err()
            .expect("create refuses a bad script");
        assert_eq!(refused.kind, ErrorKind::Content, "{script:?}");
        assert_eq!(refused.message, error.message);
    }
}

fn hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn create_refuses_an_unsupported_script() {
    let error = create_on_unsupported();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(error.message, "no such board");
}

fn create_on_unsupported() -> unemu_core_api::Error {
    let script = "unemu-test-core 1\nunsupported no such board\n";
    let config = format!(
        "unemu-test-core-config 1\nscript {} {}\n",
        hex(&digest_of(script)),
        script.len()
    );
    let source = Source {
        sha256: digest_of(script),
        bytes: script.as_bytes().to_vec(),
        as_reader: false,
    };
    CORE.create(&CreateRequest::new(
        PLATFORM,
        ConfigDocument::new(CONFIG_TYPE, config.as_bytes()),
        &source,
        Rc::new(MemoryStorage::new()),
    ))
    .err()
    .expect("refused")
}

#[test]
fn validate_refuses_other_platforms_types_and_malformed_configs() {
    let good = format!("unemu-test-core-config 1\nscript {} 5\n", "ab".repeat(32));
    let unsupported = CORE
        .validate("nes", ConfigDocument::new(CONFIG_TYPE, good.as_bytes()))
        .expect_err("another platform");
    assert_eq!(unsupported.kind, ErrorKind::Unsupported);
    let unsupported = CORE
        .validate(PLATFORM, ConfigDocument::new("text/plain", good.as_bytes()))
        .expect_err("another type");
    assert_eq!(unsupported.kind, ErrorKind::Unsupported);
    for (config, path) in [
        ("unemu-test-core-config 2\n", "line 1"),
        ("unemu-test-core-config 1\n", "line 2"),
        ("unemu-test-core-config 1\nscript zz 5\n", "line 2"),
        ("unemu-test-core-config 1\nscript {} 5\nmore\n", "line 3"),
    ] {
        let config = config.replace("{}", &"ab".repeat(32));
        let error = CORE
            .validate(
                PLATFORM,
                ConfigDocument::new(CONFIG_TYPE, config.as_bytes()),
            )
            .expect_err("malformed");
        assert_eq!(error.kind, ErrorKind::Config, "{config:?}");
        assert_eq!(error.diagnostics[0].path, path, "{config:?}");
    }
}

#[test]
fn create_takes_the_script_from_its_source_by_digest_and_refuses_options() {
    let script = "unemu-test-core 1\n";
    let config = format!(
        "unemu-test-core-config 1\nscript {} {}\n",
        hex(&digest_of(script)),
        script.len()
    );
    let elsewhere = Source {
        sha256: [7; 32],
        bytes: script.as_bytes().to_vec(),
        as_reader: false,
    };
    let document = ConfigDocument::new(CONFIG_TYPE, config.as_bytes());
    let missing = CORE
        .create(&CreateRequest::new(
            PLATFORM,
            document,
            &elsewhere,
            Rc::new(MemoryStorage::new()),
        ))
        .err()
        .expect("the source has no such digest");
    assert_eq!(missing.kind, ErrorKind::Content);
    let source = Source {
        sha256: digest_of(script),
        bytes: script.as_bytes().to_vec(),
        as_reader: true,
    };
    let options = [("speed", "2")];
    let refused = CORE
        .create(
            &CreateRequest::new(PLATFORM, document, &source, Rc::new(MemoryStorage::new()))
                .with_options(&options),
        )
        .err()
        .expect("no options");
    assert_eq!(refused.kind, ErrorKind::Config);
}

#[test]
fn the_example_of_the_crate_documentation_is_a_working_script() {
    let source = include_str!("../../src/lib.rs");
    let start = source.find("//! ```text\n").expect("the example") + "//! ```text\n".len();
    let length = source[start..].find("//! ```\n").expect("its end");
    let example: String = source[start..start + length]
        .lines()
        .map(|line| line.strip_prefix("//! ").unwrap_or("").to_owned() + "\n")
        .collect();
    let identified = identify(&example).expect("the example").expect("a script");
    assert_eq!(identified.title.as_deref(), Some("Two players"));
    let mut machine = create_on(&example, &MemoryStorage::new()).expect("a machine");
    assert_eq!(machine.description().inputs.len(), 3);
    let ten_seconds = 53_693_180;
    crate::support::run_to(machine.as_mut(), ten_seconds + 1);
    let notes = crate::support::messages(machine.as_mut());
    assert_eq!(notes[0].text, "ten seconds in");
}
