//! Configuration-state packets. Clientbound packets only implement encoding;
//! serverbound packets are decoded through [`Serverbound::decode`].

use bytes::Bytes;
use tuff_vanilla_data::packet::configuration::{Clientbound, Serverbound as Id};

use crate::auth::GameProfile;
use crate::protocol::{Encode, Packet};
use tuff_wire::{PacketReader, PacketWriter, Result};

const MAX_KNOWN_PACKS: i32 = 64;
const MAX_PLUGIN_PAYLOAD: usize = 32767;
const BRAND_CHANNEL: &str = "minecraft:brand";
const MAX_BRAND: usize = 256;

/// Login-state Login Success, which moves the client towards configuration.
pub struct LoginSuccess<'a> {
    pub profile: &'a GameProfile,
    pub session_id: u128,
}

impl Encode for LoginSuccess<'_> {
    const ID: i32 = tuff_vanilla_data::packet::login::Clientbound::LoginFinished.id();

    fn encode(&self, w: &mut PacketWriter) {
        write_game_profile(w, self.profile);
        w.write_uuid(self.session_id);
    }
}

pub fn write_game_profile(w: &mut PacketWriter, profile: &GameProfile) {
    w.write_uuid(profile.id);
    w.write_string(&profile.name);
    w.write_var_int(profile.properties.len() as i32);
    for property in &profile.properties {
        w.write_string(&property.name);
        w.write_string(&property.value);
        w.write_bool(property.signature.is_some());
        if let Some(signature) = &property.signature {
            w.write_string(signature);
        }
    }
}

#[derive(Packet)]
#[packet(id = Clientbound::CustomPayload, encode_only)]
pub struct PluginMessage<'a> {
    pub channel: &'a str,
    #[packet(rest)]
    pub data: &'a [u8],
}

/// The links vanilla knows by type; the client labels them itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownLinkType {
    BugReport = 0,
    CommunityGuidelines = 1,
    Support = 2,
    Status = 3,
    Feedback = 4,
    Community = 5,
    Website = 6,
    Forums = 7,
    News = 8,
    Announcements = 9,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServerLinkLabel {
    Known(KnownLinkType),
    Custom(tuff_item::text::TextComponent),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerLink {
    pub label: ServerLinkLabel,
    pub url: String,
}

impl ServerLink {
    pub fn known(kind: KnownLinkType, url: impl Into<String>) -> Self {
        Self {
            label: ServerLinkLabel::Known(kind),
            url: url.into(),
        }
    }

    pub fn custom(label: tuff_item::text::TextComponent, url: impl Into<String>) -> Self {
        Self {
            label: ServerLinkLabel::Custom(label),
            url: url.into(),
        }
    }
}

/// Server Links: known-typed and custom-labeled entries.
pub struct ServerLinks<'a>(pub &'a [ServerLink]);

impl Encode for ServerLinks<'_> {
    const ID: i32 = Clientbound::ServerLinks.id();

    fn encode(&self, w: &mut PacketWriter) {
        use crate::protocol::codec::WriteExt;
        w.write_var_int(self.0.len() as i32);
        for link in self.0 {
            match &link.label {
                ServerLinkLabel::Known(kind) => {
                    w.write_bool(true);
                    w.write_var_int(*kind as i32);
                }
                ServerLinkLabel::Custom(text) => {
                    w.write_bool(false);
                    w.write_text(text);
                }
            }
            w.write_string(&link.url);
        }
    }
}

/// Configuration-phase Show Dialog: the definition is bare NBT.
pub struct ShowDialog {
    pub dialog: tuff_nbt::Compound,
}

impl Encode for ShowDialog {
    const ID: i32 = Clientbound::ShowDialog.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_nbt(&tuff_nbt::Tag::Compound(self.dialog.clone()))
            .expect("in-memory NBT encodes");
    }
}

/// Crash report detail map the client attaches to its reports.
pub struct CustomReportDetails<'a>(pub &'a [(&'a str, &'a str)]);

impl Encode for CustomReportDetails<'_> {
    const ID: i32 = Clientbound::CustomReportDetails.id();

    fn encode(&self, w: &mut PacketWriter) {
        crate::protocol::custom::write_report_details(w, self.0)
            .expect("report details within vanilla limits");
    }
}

pub struct ClearDialog;

impl Encode for ClearDialog {
    const ID: i32 = Clientbound::ClearDialog.id();

    fn encode(&self, _: &mut PacketWriter) {}
}

pub struct CodeOfConduct<'a>(pub &'a str);

impl Encode for CodeOfConduct<'_> {
    const ID: i32 = Clientbound::CodeOfConduct.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_string(self.0);
    }
}

pub struct FeatureFlags<'a>(pub &'a [&'a str]);

impl Encode for FeatureFlags<'_> {
    const ID: i32 = Clientbound::UpdateEnabledFeatures.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.0.len() as i32);
        for flag in self.0 {
            w.write_string(flag);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}

pub struct KnownPacks<'a>(pub &'a [KnownPack]);

impl Encode for KnownPacks<'_> {
    const ID: i32 = Clientbound::SelectKnownPacks.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.0.len() as i32);
        for pack in self.0 {
            w.write_string(&pack.namespace);
            w.write_string(&pack.id);
            w.write_string(&pack.version);
        }
    }
}

/// Lists a registry's entries without data, so the client sources their
/// contents from a known pack.
pub struct RegistryData<'a> {
    pub registry: &'a str,
    /// Entries the client takes from its known packs.
    pub entries: &'a [&'a str],
    /// Entries sent with their data (network NBT), after `entries`.
    pub custom: &'a [(&'a str, &'a [u8])],
}

impl Encode for RegistryData<'_> {
    const ID: i32 = Clientbound::RegistryData.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_string(self.registry);
        w.write_var_int((self.entries.len() + self.custom.len()) as i32);
        for entry in self.entries {
            w.write_string(entry);
            w.write_bool(false);
        }
        for (entry, data) in self.custom {
            w.write_string(entry);
            w.write_bool(true);
            w.write_bytes(data);
        }
    }
}

/// A registry's entries as the data packs left them, in network order:
/// built-in ones by reference, pack ones with their network NBT, then
/// `extra` entries (plugin dimensions) with data.
pub struct RuntimeRegistryData<'a> {
    pub registry: &'a tuff_item::registries::RuntimeRegistry<tuff_item::registries::RawEntry>,
    pub extra: &'a [(&'a str, &'a [u8])],
}

impl Encode for RuntimeRegistryData<'_> {
    const ID: i32 = Clientbound::RegistryData.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_string(self.registry.key());
        w.write_var_int((self.registry.len() + self.extra.len()) as i32);
        for (id, entry, _) in self.registry.network_iter() {
            w.write_string(id);
            match entry.value().network.get() {
                Some(data) => {
                    w.write_bool(true);
                    w.write_bytes(data);
                }
                None => w.write_bool(false),
            }
        }
        for (entry, data) in self.extra {
            w.write_string(entry);
            w.write_bool(true);
            w.write_bytes(data);
        }
    }
}

/// Update Tags, pre-encoded because the same body is sent to every client.
#[derive(Packet)]
#[packet(id = Clientbound::UpdateTags, encode_only)]
pub struct UpdateTags(#[packet(rest)] pub Bytes);

#[derive(Packet)]
#[packet(id = Clientbound::FinishConfiguration, encode_only)]
pub struct FinishConfiguration;

#[derive(Packet)]
#[packet(id = Clientbound::KeepAlive, encode_only)]
pub struct KeepAlive(#[packet(long)] pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientInformation {
    pub view_distance: i8,
    pub chat_mode: i32,
    pub skin_parts: u8,
    pub main_hand: i32,
    pub allow_server_listings: bool,
}

impl Default for ClientInformation {
    fn default() -> Self {
        Self {
            view_distance: 10,
            chat_mode: 0,
            skin_parts: 0x7F,
            main_hand: 1,
            allow_server_listings: true,
        }
    }
}

impl ClientInformation {
    /// The client's side, with an `en_us` locale and text filtering off.
    pub fn encode(&self, w: &mut PacketWriter) {
        w.write_string("en_us");
        w.write_byte(self.view_distance);
        w.write_var_int(self.chat_mode);
        w.write_bool(true);
        w.write_unsigned_byte(self.skin_parts);
        w.write_var_int(self.main_hand);
        w.write_bool(false);
        w.write_bool(self.allow_server_listings);
        w.write_var_int(0);
    }

    pub fn decode(r: &mut PacketReader) -> Result<Self> {
        Ok(Self::decode_with_language(r)?.0)
    }

    pub fn decode_with_language(r: &mut PacketReader) -> Result<(Self, String)> {
        let language = r.read_string_chars(16)?;
        let view_distance = r.read_byte()?;
        let chat_mode = r.read_var_int()?;
        r.read_bool()?;
        let skin_parts = r.read_unsigned_byte()?;
        let main_hand = r.read_var_int()?;
        r.read_bool()?;
        let allow_server_listings = r.read_bool()?;
        r.read_var_int()?;
        Ok((
            Self {
                view_distance,
                chat_mode,
                skin_parts,
                main_hand,
                allow_server_listings,
            },
            language,
        ))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Serverbound {
    ClientInformation(ClientInformation, String),
    KnownPacks(Vec<KnownPack>),
    AcknowledgeFinish,
    AcceptCodeOfConduct,
    CookieResponse(crate::protocol::cookie::CookieResponse),
    KeepAlive(i64),
    CustomClickAction(crate::protocol::custom::CustomClickAction),
    ResourcePack {
        id: u128,
        status: Option<crate::protocol::pack::PackStatus>,
    },
    /// The client's `minecraft:brand` plugin message.
    Brand(String),
    /// Packets the server accepts but has no use for.
    Ignored,
}

impl Serverbound {
    pub fn decode(payload: Bytes) -> anyhow::Result<Self> {
        let mut r = PacketReader::new(payload);
        let id = r.read_var_int()?;
        let packet = match Id::from_id(id) {
            Some(Id::ClientInformation) => {
                let (info, language) = ClientInformation::decode_with_language(&mut r)?;
                Self::ClientInformation(info, language)
            }
            Some(Id::SelectKnownPacks) => {
                let len = r.read_var_int()?;
                anyhow::ensure!((0..=MAX_KNOWN_PACKS).contains(&len), "too many known packs");
                let mut packs = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    packs.push(KnownPack {
                        namespace: r.read_string_with_max(256)?,
                        id: r.read_string_with_max(256)?,
                        version: r.read_string_with_max(256)?,
                    });
                }
                Self::KnownPacks(packs)
            }
            Some(Id::FinishConfiguration) => Self::AcknowledgeFinish,
            Some(Id::AcceptCodeOfConduct) => Self::AcceptCodeOfConduct,
            Some(Id::CookieResponse) => {
                Self::CookieResponse(crate::protocol::cookie::CookieResponse::decode(&mut r)?)
            }
            Some(Id::KeepAlive) => Self::KeepAlive(r.read_long()?),
            Some(Id::ResourcePack) => {
                let (id, status) = crate::protocol::pack::decode_response(&mut r)?;
                Self::ResourcePack { id, status }
            }
            Some(Id::CustomClickAction) => {
                Self::CustomClickAction(crate::protocol::custom::CustomClickAction::decode(&mut r)?)
            }
            Some(Id::CustomPayload) => {
                let channel = r.read_string_with_max(256)?;
                anyhow::ensure!(
                    r.remaining() <= MAX_PLUGIN_PAYLOAD,
                    "plugin message too large"
                );
                if channel == BRAND_CHANNEL
                    && let Ok(brand) = r.read_string_with_max(MAX_BRAND)
                    && r.remaining() == 0
                {
                    return Ok(Self::Brand(brand));
                }
                return Ok(Self::Ignored);
            }
            Some(_) => return Ok(Self::Ignored),
            None => anyhow::bail!("unknown configuration packet {id:#04x}"),
        };
        anyhow::ensure!(
            r.remaining() == 0,
            "trailing bytes in configuration packet {id:#04x}"
        );
        Ok(packet)
    }

    /// The packet as a client sends it, id first; `None` for `Ignored`.
    pub fn encode(&self) -> Option<PacketWriter> {
        let mut w = PacketWriter::new();
        match self {
            Self::ClientInformation(info, _) => {
                w.write_var_int(Id::ClientInformation.id());
                info.encode(&mut w);
            }
            Self::KnownPacks(packs) => {
                w.write_var_int(Id::SelectKnownPacks.id());
                w.write_var_int(packs.len() as i32);
                for pack in packs {
                    w.write_string(&pack.namespace);
                    w.write_string(&pack.id);
                    w.write_string(&pack.version);
                }
            }
            Self::AcknowledgeFinish => w.write_var_int(Id::FinishConfiguration.id()),
            Self::AcceptCodeOfConduct => w.write_var_int(Id::AcceptCodeOfConduct.id()),
            Self::CookieResponse(response) => {
                w.write_var_int(Id::CookieResponse.id());
                response.encode(&mut w);
            }
            Self::KeepAlive(id) => {
                w.write_var_int(Id::KeepAlive.id());
                w.write_long(*id);
            }
            Self::CustomClickAction(action) => {
                w.write_var_int(Id::CustomClickAction.id());
                action.encode(&mut w);
            }
            Self::ResourcePack { id, status } => {
                let Some(status) = status else { return None };
                return Some(crate::protocol::pack::encode_response_writer(
                    *id,
                    *status,
                    Id::ResourcePack.id(),
                ));
            }
            Self::Brand(brand) => {
                w.write_var_int(Id::CustomPayload.id());
                w.write_string(BRAND_CHANNEL);
                w.write_string(brand);
            }
            Self::Ignored => return None,
        }
        Some(w)
    }
}

fn write_typed_tags<T: tuff_vanilla_data::Registry>(w: &mut PacketWriter, tags: &[(&str, &[T])]) {
    w.write_string(T::KEY);
    w.write_var_int(tags.len() as i32);
    for (name, entries) in tags {
        w.write_string(name);
        w.write_var_int(entries.len() as i32);
        for entry in *entries {
            w.write_var_int(entry.id() as i32);
        }
    }
}

fn write_data_tags(w: &mut PacketWriter, key: &str, tags: &[(&str, &[&str])]) {
    let registry = tuff_vanilla_data::data_registry::DataRegistry::get(key)
        .unwrap_or_else(|| panic!("{key} is not a data registry"));
    w.write_string(key);
    w.write_var_int(tags.len() as i32);
    for (name, entries) in tags {
        let ids: Vec<i32> = entries
            .iter()
            .filter_map(|entry| registry.entries.binary_search(entry).ok())
            .map(|id| id as i32)
            .collect();
        w.write_string(name);
        w.write_var_int(ids.len() as i32);
        for id in ids {
            w.write_var_int(id);
        }
    }
}

/// The Update Tags body for every tagged registry the client knows about,
/// with IDs matching the order of our Registry Data packets.
pub fn update_tags() -> UpdateTags {
    use tuff_vanilla_data::tag;
    type TagData = (
        &'static str,
        &'static [(&'static str, &'static [&'static str])],
    );
    let data: [TagData; 8] = [
        ("minecraft:banner_pattern", tag::banner_pattern::ALL),
        ("minecraft:damage_type", tag::damage_type::ALL),
        ("minecraft:dialog", tag::dialog::ALL),
        ("minecraft:enchantment", tag::enchantment::ALL),
        ("minecraft:instrument", tag::instrument::ALL),
        ("minecraft:painting_variant", tag::painting_variant::ALL),
        ("minecraft:timeline", tag::timeline::ALL),
        ("minecraft:worldgen/biome", tag::worldgen::biome::ALL),
    ];
    let mut w = PacketWriter::new();
    w.write_var_int(7 + data.len() as i32);
    write_typed_tags(&mut w, tag::block::ALL);
    write_typed_tags(&mut w, tag::entity_type::ALL);
    write_typed_tags(&mut w, tag::fluid::ALL);
    write_typed_tags(&mut w, tag::game_event::ALL);
    write_typed_tags(&mut w, tag::item::ALL);
    write_typed_tags(&mut w, tag::point_of_interest_type::ALL);
    write_typed_tags(&mut w, tag::potion::ALL);
    for (key, tags) in data {
        write_data_tags(&mut w, key, tags);
    }
    UpdateTags(w.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn server_links_and_code_of_conduct_encode() {
        let mut w = PacketWriter::new();
        ServerLinks(&[ServerLink::known(KnownLinkType::BugReport, "https://a.b")]).encode(&mut w);
        let bytes = w.into_bytes();
        assert_eq!(&bytes[..3], &[1, 1, 0]);
        assert_eq!(
            &bytes[3..],
            &[
                11, b'h', b't', b't', b'p', b's', b':', b'/', b'/', b'a', b'.', b'b'
            ]
        );
        let mut w = PacketWriter::new();
        ServerLinks(&[
            ServerLink::known(KnownLinkType::Website, "u"),
            ServerLink::custom(tuff_item::text::TextComponent::new("Wiki"), "v"),
        ])
        .encode(&mut w);
        let bytes = w.into_bytes();
        assert_eq!(&bytes[..4], &[2, 1, 6, 1]);
        assert_eq!(bytes[4], b'u');
        assert_eq!(bytes[5], 0);
        assert_eq!(*bytes.last().unwrap(), b'v');
        let mut w = PacketWriter::new();
        CodeOfConduct("hi").encode(&mut w);
        assert_eq!(&w.into_bytes()[..], &[2, b'h', b'i']);
    }

    #[tuff_core::watchdog]
    #[test]
    fn client_encoding_decodes_back() {
        let packets = [
            Serverbound::ClientInformation(
                ClientInformation {
                    view_distance: 32,
                    ..ClientInformation::default()
                },
                "en_us".to_owned(),
            ),
            Serverbound::KnownPacks(vec![KnownPack {
                namespace: "minecraft".into(),
                id: "core".into(),
                version: "1".into(),
            }]),
            Serverbound::AcknowledgeFinish,
            Serverbound::KeepAlive(-7),
            Serverbound::Brand("vanilla".into()),
        ];
        for packet in packets {
            let bytes = packet.encode().unwrap().into_bytes();
            assert_eq!(Serverbound::decode(bytes).unwrap(), packet);
        }
        assert!(Serverbound::Ignored.encode().is_none());
        let mut other = PacketWriter::new();
        other.write_var_int(Id::CustomPayload.id());
        other.write_string("example:channel");
        other.write_string("data");
        assert_eq!(
            Serverbound::decode(other.into_bytes()).unwrap(),
            Serverbound::Ignored
        );
        let mut long = PacketWriter::new();
        long.write_var_int(Id::CustomPayload.id());
        long.write_string(BRAND_CHANNEL);
        long.write_string(&"x".repeat(MAX_BRAND + 1));
        assert_eq!(
            Serverbound::decode(long.into_bytes()).unwrap(),
            Serverbound::Ignored
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_synchronized_registry_is_generated() {
        for registry in tuff_vanilla_data::data_registry::ALL
            .iter()
            .filter(|r| r.synchronized)
        {
            assert!(
                registry.entries.is_sorted(),
                "{} is not sorted",
                registry.key
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn unmodified_registries_encode_like_the_generated_ones() {
        let registries = tuff_item::registries::Registries::vanilla();
        let extra: &[(&str, &[u8])] = &[("a:extra", &[10, 0])];
        for registry in tuff_vanilla_data::data_registry::SYNCHRONIZED {
            let mut old = PacketWriter::new();
            RegistryData {
                registry: registry.key,
                entries: registry.entries,
                custom: extra,
            }
            .encode(&mut old);
            let mut new = PacketWriter::new();
            RuntimeRegistryData {
                registry: registries.get(registry.key).unwrap(),
                extra,
            }
            .encode(&mut new);
            assert_eq!(old.into_bytes(), new.into_bytes(), "{}", registry.key);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_changed_entry_is_sent_with_data_in_registry_order() {
        let damage =
            tuff_vanilla_data::data_registry::DataRegistry::get("minecraft:damage_type").unwrap();
        let mut loader = tuff_item::registries::Loader::default();
        loader.insert(
            damage,
            "minecraft:cactus".into(),
            "p",
            br#"{"message_id":"prick","scaling":"always","exhaustion":0.1}"#.to_vec(),
        );
        let frozen = loader.freeze().unwrap();
        let registry = frozen.get("minecraft:damage_type").unwrap();
        let mut w = PacketWriter::new();
        RuntimeRegistryData {
            registry,
            extra: &[],
        }
        .encode(&mut w);
        let mut r = PacketReader::new(w.into_bytes());
        assert_eq!(r.read_string().unwrap(), "minecraft:damage_type");
        assert_eq!(r.read_var_int().unwrap() as usize, damage.entries.len());
        for id in damage.entries {
            assert_eq!(r.read_string().unwrap(), *id);
            let has_data = r.read_bool().unwrap();
            assert_eq!(has_data, *id == "minecraft:cactus");
            if has_data {
                let data = registry.entry(id).unwrap().value().network.get().unwrap();
                assert_eq!(&r.read_bytes(data.len()).unwrap()[..], &data[..]);
            }
        }
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn update_tags_lists_fifteen_registries() {
        let body = update_tags().0;
        let mut r = PacketReader::new(body);
        assert_eq!(r.read_var_int().unwrap(), 15);
        assert_eq!(r.read_string().unwrap(), "minecraft:block");
    }
}

#[cfg(test)]
mod dialog_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn clear_dialog_has_an_empty_body_and_show_dialog_bare_nbt() {
        let mut w = PacketWriter::new();
        ClearDialog.encode(&mut w);
        assert_eq!(w.into_bytes().len(), 0);
        let mut w = PacketWriter::new();
        ShowDialog {
            dialog: tuff_nbt::Compound::default(),
        }
        .encode(&mut w);
        assert_eq!(&w.into_bytes()[..], &[10, 0]);
    }
}
