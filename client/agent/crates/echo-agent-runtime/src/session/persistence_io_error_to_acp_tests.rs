use super::io_error_to_acp;
use std::io;

#[test]
fn storage_full_maps_to_no_space_left() {
    let io = io::Error::from(io::ErrorKind::StorageFull);
    assert!(super::is_disk_full_io_error(&io));
    let acp_err = io_error_to_acp(&io);
    assert_eq!(acp_err.message, "No space left on device");
    assert_eq!(acp_err.data.unwrap()["code"], "FS_DISK_QUOTA_EXCEEDED");
}
