use super::*;
use crate::auth::{GameProfile, ProfileProperty};

#[tuff_core::watchdog]
#[test]
fn profile_snbt_encodes_id_name_and_properties() {
    let profile = GameProfile {
        id: 0x0000_0001_0000_0002_ffff_ffff_0000_0004,
        name: "Al\"x".into(),
        properties: vec![ProfileProperty {
            name: "textures".into(),
            value: "v".into(),
            signature: Some("s".into()),
        }],
    };
    assert_eq!(
        profile_snbt(&profile),
        "{id:[I;1,2,-1,4],name:\"Al\\\"x\",properties:[{name:\"textures\",value:\"v\",signature:\"s\"}]}"
    );
    let message = fetched_profile_message(&profile);
    assert_eq!(
        message.translate.as_deref(),
        Some("commands.fetchprofile.entity.success")
    );
    assert_eq!(message.with.len(), 2);
}
