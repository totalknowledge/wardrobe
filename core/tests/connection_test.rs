use std::path::PathBuf;
use wardrobe_core::{ConnectionTarget, DEFAULT_NETWORK_PORT, DriverKind};

#[test]
fn direct_disk_path_selects_embedded_connection_target() {
    let target = ConnectionTarget::parse("./data").expect("direct path should parse");

    assert_eq!(
        target,
        ConnectionTarget::EmbeddedPath(PathBuf::from("./data"))
    );
    assert_eq!(target.driver_kind(), DriverKind::Embedded);
}

#[test]
fn local_file_uris_select_embedded_connection_target() {
    assert_eq!(
        ConnectionTarget::parse("wardrobe://local/path/to/data").expect("local uri should parse"),
        ConnectionTarget::EmbeddedPath(PathBuf::from("path/to/data"))
    );
    assert_eq!(
        ConnectionTarget::parse("wardrobe+file://path/to/data").expect("file uri should parse"),
        ConnectionTarget::EmbeddedPath(PathBuf::from("path/to/data"))
    );
    assert_eq!(
        ConnectionTarget::parse("file://path/to/data").expect("plain file uri should parse"),
        ConnectionTarget::EmbeddedPath(PathBuf::from("path/to/data"))
    );
}

#[test]
fn host_connection_uri_selects_network_target_with_default_or_explicit_port() {
    let default_port_target =
        ConnectionTarget::parse("wardrobe://localhost").expect("default port target should parse");
    assert_eq!(
        default_port_target,
        ConnectionTarget::Network {
            host: "localhost".to_string(),
            port: DEFAULT_NETWORK_PORT
        }
    );
    assert_eq!(default_port_target.driver_kind(), DriverKind::Network);
    assert!(!default_port_target.requires_embedded_engine());
    assert!(default_port_target.uses_socket_transport());

    assert_eq!(
        ConnectionTarget::parse("wardrobe://localhost:24842")
            .expect("explicit port target should parse"),
        ConnectionTarget::Network {
            host: "localhost".to_string(),
            port: 24842
        }
    );
}

#[test]
fn unix_socket_uris_select_socket_target() {
    let target =
        ConnectionTarget::parse("wardrobe+unix:///tmp/wardrobe.sock").expect("target should parse");
    assert_eq!(target.driver_kind(), DriverKind::UnixSocket);
    assert!(!target.requires_embedded_engine());
    assert!(target.uses_socket_transport());

    assert_eq!(
        ConnectionTarget::parse("wardrobe://unix/tmp/wardrobe.sock")
            .expect("alternate unix target should parse")
            .driver_kind(),
        DriverKind::UnixSocket
    );
}

#[test]
fn invalid_connection_strings_return_input_errors() {
    assert!(ConnectionTarget::parse("").is_err());
    assert!(ConnectionTarget::parse("http://localhost").is_err());
    assert!(ConnectionTarget::parse("wardrobe://localhost:notaport").is_err());
    assert!(ConnectionTarget::parse("wardrobe://localhost:24842/path").is_err());
}

#[test]
fn embedded_targets_report_embedded_engine_requirement() {
    let target = ConnectionTarget::parse("./data").expect("embedded path should parse");

    assert_eq!(target.driver_kind(), DriverKind::Embedded);
    assert!(target.requires_embedded_engine());
    assert!(!target.uses_socket_transport());
}
