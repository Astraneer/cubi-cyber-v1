use core_types::{PageId, SpaceId};
use net::{ClientMessage, decode_client, encode_client};
#[test]
fn explicit_raw_lease_retirement_is_bounded_and_preserves_existing_page_requests() {
    let page = PageId {
        space: SpaceId(0),
        x: -3,
        y: 5,
        z: 7,
    };
    for message in [
        ClientMessage::ForgetPage(page),
        ClientMessage::RequestPage {
            page,
            acknowledged: None,
        },
        ClientMessage::RequestPage {
            page,
            acknowledged: Some(91),
        },
    ] {
        let bytes = encode_client(&message).unwrap();
        assert!(bytes.len() <= 44);
        assert_eq!(
            bytes[18],
            if matches!(message, ClientMessage::ForgetPage(_)) {
                21
            } else {
                3
            }
        );
        assert_eq!(decode_client(&bytes).unwrap(), message);
        for end in 0..bytes.len() {
            assert!(decode_client(&bytes[..end]).is_err());
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode_client(&trailing).is_err());
    }
}
