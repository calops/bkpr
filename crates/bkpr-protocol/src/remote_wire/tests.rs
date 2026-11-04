use super::*;
use std::io::Cursor;

fn hello() -> Hello {
    Hello {
        session_name: "日本語 / ../ exact session 🐈 ".into(),
        native_family: PROTOCOL_VERSION,
        required_capabilities: crate::wire::SUPPORTED_CAPABILITIES,
        connection_nonce: [0xa7; NONCE_LEN],
        forward_agent: true,
    }
}
fn welcome() -> Welcome {
    Welcome {
        session: SessionInfo {
            name: hello().session_name,
            session_id: "stable-session-id".into(),
            daemon_id: "current-daemon-id".into(),
        },
        connection_nonce: hello().connection_nonce,
        capabilities: crate::wire::SUPPORTED_CAPABILITIES,
        agent_socket: Some("/remote/ssh-agent.socket".into()),
        heartbeat_interval_ms: 1000,
        lease_timeout_ms: 5000,
    }
}
fn encoded(control: &Control) -> Vec<u8> {
    Codec::new().encode(control).unwrap().to_vec()
}
fn root(bytes: &[u8]) -> usize {
    u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize
}
fn table_field(bytes: &[u8], table: usize, slot: usize) -> (usize, usize) {
    let backwards = i32::from_le_bytes(bytes[table..table + 4].try_into().unwrap());
    let vtable = usize::try_from(table as i64 - i64::from(backwards)).unwrap();
    let entry = vtable + 4 + slot * 2;
    let field = table + u16::from_le_bytes(bytes[entry..entry + 2].try_into().unwrap()) as usize;
    (entry, field)
}
fn indirect(bytes: &[u8], field: usize) -> usize {
    field + u32::from_le_bytes(bytes[field..field + 4].try_into().unwrap()) as usize
}
fn body(bytes: &[u8]) -> usize {
    indirect(bytes, table_field(bytes, root(bytes), 1).1)
}

#[test]
fn all_controls_roundtrip_and_codec_storage_is_reusable() {
    let mut codec = Codec::new();
    for control in [
        Control::Hello(hello()),
        Control::Welcome(welcome()),
        Control::Welcome(Welcome {
            agent_socket: None,
            ..welcome()
        }),
        Control::Failure(Failure::new("no existing session 日本語")),
        Control::Ping { sequence: 0 },
        Control::Ping { sequence: u64::MAX },
        Control::Pong { sequence: u64::MAX },
        Control::Close,
    ] {
        let bytes = codec.encode(&control).unwrap();
        assert_eq!(decode(bytes).unwrap(), control);
    }
}

#[cfg(unix)]
#[test]
fn remote_agent_path_preserves_raw_bytes_and_debug_does_not_disclose_it() {
    use std::os::unix::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_vec(
        b"/remote/private-\xff.sock".to_vec(),
    ));
    let control = Control::Welcome(Welcome {
        agent_socket: Some(path),
        ..welcome()
    });
    assert_eq!(decode(&encoded(&control)).unwrap(), control);
    assert!(!format!("{control:?}").contains("private-"));
    assert!(!format!("{control:?}").contains("remote/"));
}

#[test]
fn welcome_is_bound_to_exact_requested_name_nonce_caps_and_forwarding_consent() {
    welcome().validate_for(&hello()).unwrap();
    let mut changed = welcome();
    changed.session.name.push(' ');
    assert!(changed.validate_for(&hello()).is_err());
    changed = welcome();
    changed.connection_nonce[0] ^= 1;
    assert!(changed.validate_for(&hello()).is_err());
    changed = welcome();
    changed.capabilities = 0;
    assert!(changed.validate_for(&hello()).is_err());
    let no_forward = Hello {
        forward_agent: false,
        ..hello()
    };
    assert!(welcome().validate_for(&no_forward).is_err());
    Welcome {
        agent_socket: None,
        ..welcome()
    }
    .validate_for(&no_forward)
    .unwrap();
    assert!(
        Welcome {
            agent_socket: None,
            ..welcome()
        }
        .validate_for(&hello())
        .is_err()
    );
}

#[test]
fn admission_rejects_bad_headers_without_reading_declared_payload() {
    let payload = encoded(&Control::Close);
    let valid = encode_frame(&payload).unwrap();
    assert_eq!(
        frame_payload_len(&valid[..HEADER_LEN]).unwrap(),
        payload.len()
    );
    let mut invalid = Vec::new();
    let mut bad = valid.clone();
    bad[0] = b'X';
    invalid.push(bad);
    let mut bad = valid.clone();
    bad[4..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
    invalid.push(bad);
    let mut bad = valid.clone();
    bad[12] = 1;
    invalid.push(bad);
    for length in [0, 3, MAX_CONTROL as u32 + 1, u32::MAX] {
        let mut bad = valid.clone();
        bad[8..12].copy_from_slice(&length.to_le_bytes());
        invalid.push(bad);
    }
    for bytes in invalid {
        let mut reader = Cursor::new(bytes);
        assert!(read_frame(&mut reader).is_err());
        assert_eq!(reader.position(), HEADER_LEN as u64);
    }
    for length in 0..HEADER_LEN {
        assert!(frame_payload_len(&valid[..length]).is_err());
    }
    assert!(frame_payload_len(&valid[..HEADER_LEN + 1]).is_err());
    assert!(decode(&vec![0; MAX_CONTROL + 1]).is_err());
    assert!(encode_frame(&vec![0; MAX_CONTROL + 1]).is_err());
}

#[test]
fn framed_truncation_and_coalescing_preserve_message_boundaries() {
    let first = Control::Hello(hello());
    let second = Control::Pong { sequence: 97 };
    let frame = encode_frame(&encoded(&first)).unwrap();
    for length in 0..frame.len() {
        assert!(
            read_frame(&mut Cursor::new(&frame[..length])).is_err(),
            "length {length}"
        );
    }
    let mut joined = frame;
    write_frame(&mut joined, &encoded(&second)).unwrap();
    let mut cursor = Cursor::new(joined);
    assert_eq!(decode(&read_frame(&mut cursor).unwrap()).unwrap(), first);
    assert_eq!(decode(&read_frame(&mut cursor).unwrap()).unwrap(), second);
    assert_eq!(cursor.position(), cursor.get_ref().len() as u64);
}

#[test]
fn missing_or_unknown_control_union_is_never_an_implicit_close() {
    let valid = encoded(&Control::Ping { sequence: 1 });
    for discriminator in [0, 255] {
        let mut malformed = valid.clone();
        let tag = table_field(&malformed, root(&malformed), 0).1;
        malformed[tag] = discriminator;
        assert!(decode(&malformed).is_err());
    }
    let mut malformed = valid.clone();
    let entry = table_field(&malformed, root(&malformed), 1).0;
    malformed[entry..entry + 2].fill(0);
    assert!(decode(&malformed).is_err());
    let mut malformed = valid;
    malformed[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode(&malformed).is_err());
}

#[test]
fn present_malformed_agent_wrapper_cannot_become_an_absent_agent() {
    let valid = encoded(&Control::Welcome(welcome()));
    let wrapper = indirect(&valid, table_field(&valid, body(&valid), 3).1);
    for discriminator in [0, 1, 255] {
        let mut malformed = valid.clone();
        let tag = table_field(&malformed, wrapper, 0).1;
        let entry = table_field(&malformed, wrapper, 1).0;
        malformed[tag] = discriminator;
        malformed[entry..entry + 2].fill(0);
        assert!(decode(&malformed).is_err(), "discriminator {discriminator}");
    }
    let mut malformed = valid.clone();
    let tag = table_field(&malformed, wrapper, 0).1;
    malformed[tag] = 255;
    assert!(decode(&malformed).is_err());
    // Genuine outer absence is the only valid no-agent representation.
    let mut absent = valid;
    let entry = table_field(&absent, body(&absent), 3).0;
    absent[entry..entry + 2].fill(0);
    let Control::Welcome(decoded) = decode(&absent).unwrap() else {
        panic!("expected welcome")
    };
    assert_eq!(decoded.agent_socket, None);
}

#[test]
fn missing_required_session_and_nonce_fields_fail_at_consumer_boundary() {
    for (control, slots) in [
        (Control::Hello(hello()), vec![0, 3]),
        (Control::Welcome(welcome()), vec![0, 1]),
    ] {
        let valid = encoded(&control);
        for slot in slots {
            let mut malformed = valid.clone();
            let entry = table_field(&malformed, body(&malformed), slot).0;
            malformed[entry..entry + 2].fill(0);
            assert!(decode(&malformed).is_err());
        }
    }
}

fn raw_hello(name: &str, family: u32, caps: u64, nonce: &[u8]) -> Vec<u8> {
    let mut builder = Builder::new();
    let name = builder.create_string(name);
    let nonce = builder.create_vector(nonce);
    let hello = g::Hello::create(&mut builder, name, family, caps, nonce, false);
    let value = g::Control::create_hello(&mut builder, hello);
    let root = g::Message::create(&mut builder, value);
    builder.finish(root, None).to_vec()
}

#[test]
fn semantic_bounds_are_enforced_for_untrusted_wire_not_only_local_encoders() {
    for bytes in [
        raw_hello("", PROTOCOL_VERSION, 0, &[0; NONCE_LEN]),
        raw_hello("x\n", PROTOCOL_VERSION, 0, &[0; NONCE_LEN]),
        raw_hello(
            &"x".repeat(MAX_SESSION_NAME + 1),
            PROTOCOL_VERSION,
            0,
            &[0; NONCE_LEN],
        ),
        raw_hello("name", PROTOCOL_VERSION + 1, 0, &[0; NONCE_LEN]),
        raw_hello("name", PROTOCOL_VERSION, 1 << 63, &[0; NONCE_LEN]),
        raw_hello("name", PROTOCOL_VERSION, 0, &[]),
        raw_hello("name", PROTOCOL_VERSION, 0, &[0; NONCE_LEN - 1]),
        raw_hello("name", PROTOCOL_VERSION, 0, &[0; NONCE_LEN + 1]),
    ] {
        assert!(decode(&bytes).is_err());
    }
    let valid = raw_hello(
        &"x".repeat(MAX_SESSION_NAME),
        PROTOCOL_VERSION,
        0,
        &[0; NONCE_LEN],
    );
    assert!(decode(&valid).is_ok());
    let mut non_utf8 = valid;
    let name = indirect(&non_utf8, table_field(&non_utf8, body(&non_utf8), 0).1);
    non_utf8[name + 4] = 255;
    assert!(decode(&non_utf8).is_err());
}

#[test]
fn malformed_nonce_vector_offsets_lengths_and_truncation_are_rejected() {
    let valid = encoded(&Control::Hello(hello()));
    let nonce_field = table_field(&valid, body(&valid), 3).1;
    let nonce = indirect(&valid, nonce_field);
    let mut bad = valid.clone();
    bad[nonce_field..nonce_field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode(&bad).is_err());
    let mut bad = valid.clone();
    bad[nonce..nonce + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode(&bad).is_err());
    assert!(decode(&valid[..nonce + 4 + NONCE_LEN - 1]).is_err());
}

#[test]
fn welcome_identity_path_and_lease_bounds_fail_closed() {
    let mut invalid = vec![
        Welcome {
            heartbeat_interval_ms: MIN_HEARTBEAT_MS - 1,
            ..welcome()
        },
        Welcome {
            heartbeat_interval_ms: MAX_HEARTBEAT_MS + 1,
            ..welcome()
        },
        Welcome {
            lease_timeout_ms: 2999,
            ..welcome()
        },
        Welcome {
            lease_timeout_ms: MAX_LEASE_TIMEOUT_MS + 1,
            ..welcome()
        },
        Welcome {
            agent_socket: Some(PathBuf::new()),
            ..welcome()
        },
        Welcome {
            agent_socket: Some("x".repeat(MAX_AGENT_PATH + 1).into()),
            ..welcome()
        },
    ];
    let mut empty_id = welcome();
    empty_id.session.session_id.clear();
    invalid.push(empty_id);
    let mut long_id = welcome();
    long_id.session.daemon_id = "x".repeat(MAX_IDENTITY + 1);
    invalid.push(long_id);
    for welcome in invalid {
        assert!(Codec::new().encode(&Control::Welcome(welcome)).is_err());
    }
    let valid = encoded(&Control::Welcome(welcome()));
    for (slot, value) in [(4, 0_u32), (5, u32::MAX)] {
        let mut malformed = valid.clone();
        let field = table_field(&malformed, body(&malformed), slot).1;
        malformed[field..field + 4].copy_from_slice(&value.to_le_bytes());
        assert!(decode(&malformed).is_err());
    }
}

#[cfg(unix)]
#[test]
fn embedded_nul_remote_agent_socket_is_not_a_usable_path() {
    use std::os::unix::ffi::OsStringExt;
    let path = std::ffi::OsString::from_vec(b"/remote/agent\0other".to_vec());
    assert!(
        Codec::new()
            .encode(&Control::Welcome(Welcome {
                agent_socket: Some(path.into()),
                ..welcome()
            }))
            .is_err()
    );
    let mut malformed = encoded(&Control::Welcome(welcome()));
    let wrapper = indirect(&malformed, table_field(&malformed, body(&malformed), 3).1);
    let path = indirect(&malformed, table_field(&malformed, wrapper, 1).1);
    let bytes = indirect(&malformed, table_field(&malformed, path, 0).1);
    malformed[bytes + 4] = 0;
    assert!(decode(&malformed).is_err());
}

#[test]
fn failure_diagnostics_are_bounded_single_line_utf8() {
    let failure = Failure::new(format!("\u{1b}[31m\n\0{}", "日本語".repeat(MAX_FAILURE)));
    assert!(failure.message.len() <= MAX_FAILURE);
    assert!(!failure.message.chars().any(char::is_control));
    let control = Control::Failure(failure);
    assert_eq!(decode(&encoded(&control)).unwrap(), control);
    assert!(!Failure::new("").message.is_empty());
    for message in [String::new(), "a\nb".into(), "x".repeat(MAX_FAILURE + 1)] {
        assert!(
            Codec::new()
                .encode(&Control::Failure(Failure { message }))
                .is_err()
        );
    }
    let mut builder = Builder::new();
    let message = builder.create_string("x".repeat(MAX_FAILURE + 1));
    let failure = g::Failure::create(&mut builder, message);
    let value = g::Control::create_failure(&mut builder, failure);
    let root = g::Message::create(&mut builder, value);
    assert!(decode(builder.finish(root, None)).is_err());
}
